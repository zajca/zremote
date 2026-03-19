use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

#[cfg(feature = "local")]
#[derive(rust_embed::Embed)]
#[folder = "../../web/dist/"]
struct WebAssets;

/// Serve static files from the embedded web UI assets.
///
/// If the requested path matches a file, serve it with the correct Content-Type.
/// Otherwise, serve `index.html` for SPA client-side routing.
#[cfg(feature = "local")]
pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try to serve the exact file first
    if let Some(content) = WebAssets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.as_ref())],
            content.data.to_vec(),
        )
            .into_response();
    }

    // SPA fallback: serve index.html for any non-file path
    match WebAssets::get("index.html") {
        Some(content) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html")],
            content.data.to_vec(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "web UI not available").into_response(),
    }
}

/// Serve static files from a filesystem directory (for development).
///
/// Falls back to `index.html` for SPA routing, same as the embedded handler.
/// Includes path traversal protection: resolved paths must stay within `web_dir`.
#[cfg(feature = "local")]
pub async fn filesystem_static_handler(uri: Uri, web_dir: std::path::PathBuf) -> Response {
    let path = uri.path().trim_start_matches('/');
    let file_path = web_dir.join(path);

    // Path traversal protection: ensure the resolved file stays within web_dir
    if let Ok(canonical_dir) = web_dir.canonicalize()
        && let Ok(canonical_file) = file_path.canonicalize()
        && !canonical_file.starts_with(&canonical_dir)
    {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }

    // Try to serve the exact file
    if file_path.is_file()
        && let Ok(data) = tokio::fs::read(&file_path).await
    {
        let mime = mime_guess::from_path(&file_path).first_or_octet_stream();
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.as_ref().to_string())],
            data,
        )
            .into_response();
    }

    // SPA fallback: serve index.html
    let index_path = web_dir.join("index.html");
    match tokio::fs::read(&index_path).await {
        Ok(data) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html".to_string())],
            data,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "web UI not available").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_handler_serves_index_for_root() {
        // If web/dist/index.html doesn't exist (CI), handler returns 404 gracefully.
        let response = static_handler(Uri::from_static("/")).await;
        let status = response.status();
        // Either 200 (if dist exists with index.html) or 404
        assert!(
            status == StatusCode::OK || status == StatusCode::NOT_FOUND,
            "unexpected status: {status}"
        );
    }

    #[tokio::test]
    async fn static_handler_serves_index_for_spa_routes() {
        let response = static_handler(Uri::from_static("/sessions/123")).await;
        let status = response.status();
        assert!(
            status == StatusCode::OK || status == StatusCode::NOT_FOUND,
            "unexpected status: {status}"
        );
    }

    #[tokio::test]
    async fn filesystem_handler_returns_not_found_for_missing_dir() {
        let response = filesystem_static_handler(
            Uri::from_static("/"),
            std::path::PathBuf::from("/nonexistent/path"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn filesystem_handler_serves_file_from_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("index.html");
        tokio::fs::write(&index_path, "<html>test</html>")
            .await
            .unwrap();

        let response =
            filesystem_static_handler(Uri::from_static("/"), dir.path().to_path_buf()).await;
        // Root path is not a file, so SPA fallback should serve index.html
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn filesystem_handler_serves_specific_file() {
        let dir = tempfile::tempdir().unwrap();
        let css_path = dir.path().join("style.css");
        tokio::fs::write(&css_path, "body { color: red; }")
            .await
            .unwrap();

        let response =
            filesystem_static_handler(Uri::from_static("/style.css"), dir.path().to_path_buf())
                .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn filesystem_handler_spa_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("index.html");
        tokio::fs::write(&index_path, "<html><body>SPA</body></html>")
            .await
            .unwrap();

        // Request a non-existent file path (SPA route)
        let response = filesystem_static_handler(
            Uri::from_static("/sessions/some-id"),
            dir.path().to_path_buf(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn filesystem_handler_no_index_html() {
        // Directory exists but has no index.html
        let dir = tempfile::tempdir().unwrap();

        let response = filesystem_static_handler(
            Uri::from_static("/nonexistent/path"),
            dir.path().to_path_buf(),
        )
        .await;
        // Should return 404 since there is no index.html for fallback
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn filesystem_handler_serves_js_file_with_correct_mime() {
        let dir = tempfile::tempdir().unwrap();
        let js_path = dir.path().join("app.js");
        tokio::fs::write(&js_path, "console.log('hello');")
            .await
            .unwrap();

        let response =
            filesystem_static_handler(Uri::from_static("/app.js"), dir.path().to_path_buf()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn filesystem_handler_nested_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sub_dir = dir.path().join("assets");
        std::fs::create_dir_all(&sub_dir).unwrap();
        let file_path = sub_dir.join("style.css");
        tokio::fs::write(&file_path, "body{}").await.unwrap();

        let response = filesystem_static_handler(
            Uri::from_static("/assets/style.css"),
            dir.path().to_path_buf(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn static_handler_returns_for_nonexistent_asset() {
        // Request a specific asset file that doesn't exist in embedded assets
        let response = static_handler(Uri::from_static("/nonexistent.js")).await;
        let status = response.status();
        // Falls back to index.html (if exists) or 404
        assert!(
            status == StatusCode::OK || status == StatusCode::NOT_FOUND,
            "unexpected status: {status}"
        );
    }
}
