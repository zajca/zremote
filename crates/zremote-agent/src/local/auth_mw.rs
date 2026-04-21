//! Bearer-auth middleware for local-mode routes (RFC auth-overhaul Phase 6).
//!
//! Applied to every REST and WebSocket route except `/health` and
//! `/api/mode`. WebSocket upgrades may authenticate via
//! `Authorization: Bearer` or `?token=` query param. REST routes require
//! `Authorization: Bearer` — query-param auth is rejected.
//!
//! The `?token=` fallback exists solely for WebSocket handshakes, because
//! browsers and GPUI's WS client cannot attach arbitrary headers to the
//! upgrade request. Accepting it on REST routes would leak the bearer into
//! server logs, browser history, and referrer headers.
//!
//! Failure returns an opaque `401 { "error": "unauthorized" }` — no
//! `WWW-Authenticate: Bearer`, no reason field — to avoid leaking whether
//! the caller was missing the header, used the wrong scheme, or supplied a
//! bad token. Matches the server-mode collapse in
//! `zremote_server::auth_mw`.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header::AUTHORIZATION, header::UPGRADE};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::state::LocalAppState;
use super::token::verify_constant_time;

/// Axum middleware that gates a router on the agent's `local_token`.
///
/// Accepts the token via:
/// - `Authorization: Bearer <token>` header (all routes), or
/// - `?token=<token>` query param (WebSocket upgrade requests ONLY — REST
///   requests with only a query-param token are rejected).
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

    // Query-param fallback: accepted ONLY for WebSocket upgrade requests.
    // Checking `Upgrade: websocket` keeps the bearer out of logs / history
    // for every REST route.
    let is_ws_upgrade = request.headers().get(UPGRADE).is_some_and(|v| {
        v.to_str()
            .is_ok_and(|s| s.eq_ignore_ascii_case("websocket"))
    });

    if is_ws_upgrade {
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
    }

    unauthorized()
}

/// Extract the `token` query parameter from a URL query string, if present.
/// Returns the URL-decoded token or `None` if the key is absent.
fn parse_token_query_param(query: &str) -> Option<String> {
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
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

    #[test]
    fn parse_token_query_param_skips_valueless_keys() {
        // A valueless key like `foo&` must not short-circuit the search — we
        // must still find `token=abc` later in the string.
        assert_eq!(
            parse_token_query_param("foo&token=abc"),
            Some("abc".to_string())
        );
    }
}
