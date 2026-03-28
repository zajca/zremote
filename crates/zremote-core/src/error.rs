#[cfg(feature = "axum")]
use axum::extract::FromRequest;
#[cfg(feature = "axum")]
use axum::extract::rejection::JsonRejection;
#[cfg(feature = "axum")]
use axum::http::StatusCode;
#[cfg(feature = "axum")]
use axum::response::{IntoResponse, Response};
#[cfg(feature = "axum")]
use serde::de::DeserializeOwned;

/// Application error type for the server.
#[derive(Debug)]
pub enum AppError {
    Database(sqlx::Error),
    NotFound(String),
    Unauthorized(String),
    BadRequest(String),
    Conflict(String),
    Internal(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(e) => write!(f, "database error: {e}"),
            Self::NotFound(msg) => write!(f, "not found: {msg}"),
            Self::Unauthorized(msg) => write!(f, "unauthorized: {msg}"),
            Self::BadRequest(msg) => write!(f, "bad request: {msg}"),
            Self::Conflict(msg) => write!(f, "conflict: {msg}"),
            Self::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

#[cfg(feature = "axum")]
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            Self::Database(e) => {
                tracing::error!(error = %e, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "DATABASE_ERROR",
                    "internal database error".to_string(),
                )
            }
            Self::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg.clone()),
            Self::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg.clone()),
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg.clone()),
            Self::Conflict(msg) => (StatusCode::CONFLICT, "CONFLICT", msg.clone()),
            Self::Internal(msg) => {
                tracing::error!(error = %msg, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "internal server error".to_string(),
                )
            }
        };

        let body = serde_json::json!({
            "error": {
                "code": code,
                "message": message,
            }
        });

        (status, axum::Json(body)).into_response()
    }
}

/// Custom JSON extractor that converts parse errors to the standard
/// `{"error": {"code": "BAD_REQUEST", "message": "..."}}` format.
#[cfg(feature = "axum")]
pub struct AppJson<T>(pub T);

#[cfg(feature = "axum")]
impl<S, T> FromRequest<S> for AppJson<T>
where
    axum::Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(AppError::BadRequest(rejection.body_text())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_sqlx_error() {
        let sqlx_err = sqlx::Error::Configuration("test".into());
        let app_err = AppError::from(sqlx_err);
        assert!(matches!(app_err, AppError::Database(_)));
    }

    #[test]
    fn display_format() {
        assert_eq!(
            AppError::NotFound("x".to_string()).to_string(),
            "not found: x"
        );
        assert_eq!(
            AppError::Unauthorized("x".to_string()).to_string(),
            "unauthorized: x"
        );
        assert_eq!(
            AppError::BadRequest("x".to_string()).to_string(),
            "bad request: x"
        );
        assert_eq!(
            AppError::Internal("x".to_string()).to_string(),
            "internal error: x"
        );
    }

    #[test]
    fn display_database_error() {
        let db_err = sqlx::Error::Configuration("conn failed".into());
        let app_err = AppError::Database(db_err);
        let msg = app_err.to_string();
        assert!(msg.starts_with("database error:"), "got: {msg}");
    }

    #[test]
    fn display_conflict_error() {
        assert_eq!(
            AppError::Conflict("dup".to_string()).to_string(),
            "conflict: dup"
        );
    }
}

#[cfg(all(test, feature = "axum"))]
mod axum_tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn extract_error_response(error: AppError) -> (StatusCode, serde_json::Value) {
        let response = error.into_response();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        (status, json)
    }

    #[tokio::test]
    async fn not_found_returns_404_with_message() {
        let (status, json) =
            extract_error_response(AppError::NotFound("host 123 not found".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["error"]["code"], "NOT_FOUND");
        assert_eq!(json["error"]["message"], "host 123 not found");
    }

    #[tokio::test]
    async fn unauthorized_returns_401_with_message() {
        let (status, json) =
            extract_error_response(AppError::Unauthorized("invalid token".to_string())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["error"]["code"], "UNAUTHORIZED");
        assert_eq!(json["error"]["message"], "invalid token");
    }

    #[tokio::test]
    async fn bad_request_returns_400_with_message() {
        let (status, json) =
            extract_error_response(AppError::BadRequest("invalid host ID".to_string())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "BAD_REQUEST");
        assert_eq!(json["error"]["message"], "invalid host ID");
    }

    #[tokio::test]
    async fn internal_returns_500_with_generic_message() {
        let (status, json) =
            extract_error_response(AppError::Internal("secret details".to_string())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["error"]["code"], "INTERNAL_ERROR");
        // Should NOT leak the internal message
        assert_eq!(json["error"]["message"], "internal server error");
    }

    #[tokio::test]
    async fn database_error_returns_500_with_generic_message() {
        // Create a sqlx error by trying to parse an invalid connection string
        let db_err = sqlx::Error::Configuration("test error".into());
        let (status, json) = extract_error_response(AppError::Database(db_err)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["error"]["code"], "DATABASE_ERROR");
        // Should NOT leak the database error details
        assert_eq!(json["error"]["message"], "internal database error");
    }

    #[tokio::test]
    async fn conflict_returns_409_with_message() {
        let (status, json) =
            extract_error_response(AppError::Conflict("resource already exists".to_string())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"]["code"], "CONFLICT");
        assert_eq!(json["error"]["message"], "resource already exists");
    }
}
