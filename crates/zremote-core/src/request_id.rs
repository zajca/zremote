use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;
use uuid::Uuid;

/// Key for storing the request ID in request/response extensions.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// Middleware that generates a UUID v4 request ID for each request.
///
/// The ID is:
/// - Stored in request extensions as `RequestId` (accessible to handlers)
/// - Added to the response as the `x-request-id` header
/// - Recorded in a tracing span so all downstream logs include it
pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let id = Uuid::new_v4().to_string();

    request.extensions_mut().insert(RequestId(id.clone()));

    let span = tracing::info_span!("request", request_id = %id);
    let mut response = next.run(request).instrument(span).await;

    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert("x-request-id", value);
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn echo_request_id(request: Request) -> String {
        request
            .extensions()
            .get::<RequestId>()
            .map_or_else(|| "none".to_string(), |r| r.0.clone())
    }

    fn build_app() -> Router {
        Router::new()
            .route("/test", get(echo_request_id))
            .layer(axum::middleware::from_fn(request_id_middleware))
    }

    fn test_request() -> Request<Body> {
        Request::builder().uri("/test").body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn adds_request_id_header_to_response() {
        let app = build_app();
        let response = app.oneshot(test_request()).await.unwrap();

        // Response has x-request-id header
        let header_value = response
            .headers()
            .get("x-request-id")
            .expect("response should have x-request-id header")
            .to_str()
            .unwrap()
            .to_string();

        assert!(
            Uuid::parse_str(&header_value).is_ok(),
            "x-request-id should be a valid UUID, got: {header_value}"
        );

        // Body should echo the same request ID from extensions
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(body_str, header_value);
    }

    #[tokio::test]
    async fn each_request_gets_unique_id() {
        let app1 = build_app();
        let resp1 = app1.oneshot(test_request()).await.unwrap();
        let id1 = resp1
            .headers()
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let app2 = build_app();
        let resp2 = app2.oneshot(test_request()).await.unwrap();
        let id2 = resp2
            .headers()
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        assert_ne!(id1, id2, "each request should get a unique ID");
        assert!(Uuid::parse_str(&id1).is_ok());
        assert!(Uuid::parse_str(&id2).is_ok());
    }
}
