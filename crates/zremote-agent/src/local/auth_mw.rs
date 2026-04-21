//! Bearer-auth middleware for local-mode routes (RFC auth-overhaul Phase 6).
//!
//! Applied to every REST and WebSocket route except `/health` and
//! `/api/mode`. Compares `Authorization: Bearer <token>` against the agent's
//! `local_token` in constant time. WebSocket upgrade requests accept the
//! token via `?token=` query param when `require_admin_token` is set,
//! because GPUI's WS client can't add arbitrary headers to the handshake.
//!
//! Failure returns an opaque `401 { "error": "unauthorized" }` — no
//! `WWW-Authenticate: Bearer`, no reason field — to avoid leaking whether
//! the caller was missing the header, used the wrong scheme, or supplied a
//! bad token. Matches the server-mode collapse in
//! `zremote_server::auth_mw`.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header::AUTHORIZATION};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::state::LocalAppState;
use super::token::verify_constant_time;

/// Axum middleware that gates a router on the agent's `local_token`.
///
/// Accepts the token via:
/// - `Authorization: Bearer <token>` header (preferred), or
/// - `?token=<token>` query param (fallback for WebSocket upgrades, where
///   the client can't attach arbitrary headers).
///
/// Returns `401 { "error": "unauthorized" }` on any failure.
pub(crate) async fn require_local_token(
    State(state): State<Arc<LocalAppState>>,
    request: Request,
    next: Next,
) -> Response {
    let expected = state.local_token.as_str();

    // Prefer the header. Accept lower- and upper-case scheme; trim whitespace.
    let header_tok = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.strip_prefix("Bearer ")
                .or_else(|| s.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|t| !t.is_empty());

    if let Some(tok) = header_tok
        && verify_constant_time(tok, expected)
    {
        return next.run(request).await;
    }

    // Fallback: `?token=<token>` query param. Used by WebSocket upgrade
    // requests because browsers / GPUI can't add headers to WS handshakes.
    let query_tok = request
        .uri()
        .query()
        .and_then(parse_token_query_param)
        .filter(|t| !t.is_empty());

    if let Some(tok) = query_tok
        && verify_constant_time(&tok, expected)
    {
        return next.run(request).await;
    }

    unauthorized()
}

/// Extract the `token` query parameter from a URL query string, if present.
/// Returns the URL-decoded token or `None` if the key is absent.
fn parse_token_query_param(query: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == "token" {
            return Some(url_decode(v));
        }
    }
    None
}

/// Minimal URL-decoder for the token query param. Handles `%XX` escapes and
/// `+` → space. Invalid escapes are dropped (defensive — a malformed token
/// would fail the constant-time compare anyway).
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &bytes[i + 1..i + 3];
                if let Ok(hex_str) = std::str::from_utf8(hex)
                    && let Ok(byte) = u8::from_str_radix(hex_str, 16)
                {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_finds_token() {
        assert_eq!(
            parse_token_query_param("token=abc123"),
            Some("abc123".to_string())
        );
        assert_eq!(
            parse_token_query_param("foo=bar&token=xyz"),
            Some("xyz".to_string())
        );
        assert_eq!(
            parse_token_query_param("token=a%2Bb"),
            Some("a+b".to_string())
        );
    }

    #[test]
    fn parse_query_missing_token() {
        assert_eq!(parse_token_query_param("foo=bar"), None);
        assert_eq!(parse_token_query_param(""), None);
    }
}
