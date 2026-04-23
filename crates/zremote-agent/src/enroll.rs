//! `zremote agent enroll` — one-shot enrollment: exchange a code for a durable
//! ed25519 identity, persist it via keyring (primary) or file fallback.
//!
//! **Flow:**
//! 1. Generate a fresh ed25519 keypair.
//! 2. POST `/api/enroll` with `{ enrollment_code, hostname, public_key }`.
//! 3. On 201 Created: receive `{ agent_id, session_token }`.
//! 4. Persist `agent_id` + signing-key bytes via [`crate::config::CredentialStore`].
//! 5. Exit 0 on success, 1 on rejection.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::SigningKey;
use rand::TryRngCore;
use serde::{Deserialize, Serialize};

use crate::config::{CredentialStore, StoreError};

/// Errors during enrollment.
#[derive(Debug)]
pub enum EnrollError {
    KeyGen,
    Network(reqwest::Error),
    ServerRejected(String),
    PersistFailed(StoreError),
}

impl std::fmt::Display for EnrollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeyGen => write!(f, "failed to generate ed25519 keypair (CSPRNG unavailable)"),
            Self::Network(e) => write!(f, "HTTP request failed: {e}"),
            Self::ServerRejected(msg) => write!(f, "server rejected enrollment: {msg}"),
            Self::PersistFailed(e) => write!(f, "failed to persist credentials: {e}"),
        }
    }
}

impl std::error::Error for EnrollError {}

#[derive(Serialize)]
struct EnrollRequest<'a> {
    enrollment_code: &'a str,
    hostname: &'a str,
    public_key: &'a str,
}

#[derive(Deserialize)]
struct EnrollResponse {
    agent_id: String,
    // Reserved for forward compat; agent authenticates via WS handshake post-enroll.
    #[allow(dead_code)]
    session_token: Option<String>,
}

/// Run the enrollment flow. Exits 0 on success, returns Err on failure.
pub async fn run_enroll(
    code: &str,
    server: &str,
    key_file: Option<PathBuf>,
) -> Result<(), EnrollError> {
    // Step 1: generate signing keypair.
    let mut seed = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut seed)
        .map_err(|_| EnrollError::KeyGen)?;
    let signing_key = SigningKey::from_bytes(&seed);
    let public_key_b64 = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());

    // Step 2: POST /api/enroll.
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".to_string());

    let base_url = server.trim_end_matches('/');
    let url = format!("{base_url}/api/enroll");

    // No redirects: a 3xx would silently forward enrollment_code + public_key
    // to an attacker-controlled host. Timeouts prevent indefinite hangs.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(EnrollError::Network)?;

    let resp = client
        .post(&url)
        .json(&EnrollRequest {
            enrollment_code: code,
            hostname: &hostname,
            public_key: &public_key_b64,
        })
        .send()
        .await
        .map_err(EnrollError::Network)?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(EnrollError::ServerRejected(format!(
            "HTTP {status}: {body}"
        )));
    }

    // Step 3: parse response (bounded to 8 KiB to prevent OOM on hostile servers).
    const MAX_ENROLL_RESPONSE: usize = 8192;
    let bytes = resp.bytes().await.map_err(EnrollError::Network)?;
    if bytes.len() > MAX_ENROLL_RESPONSE {
        return Err(EnrollError::ServerRejected(format!(
            "response body too large ({} bytes, max {MAX_ENROLL_RESPONSE})",
            bytes.len()
        )));
    }
    let enroll_resp: EnrollResponse =
        serde_json::from_slice(&bytes).map_err(|e| EnrollError::ServerRejected(e.to_string()))?;
    let agent_id = enroll_resp.agent_id;

    // Step 4: persist credentials.
    let store = CredentialStore::new(key_file);
    store
        .save(&agent_id, &signing_key)
        .map_err(EnrollError::PersistFailed)?;

    tracing::info!(agent_id = %agent_id, hostname = %hostname, "enrollment succeeded");
    eprintln!("Enrolled successfully. agent_id={agent_id}");

    Ok(())
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    /// Enroll with a mock HTTP server; credentials must be persisted to file
    /// with mode 0600. Sets ZREMOTE_ALLOW_FILE_KEY_FALLBACK=1 because CI has no keyring.
    #[tokio::test]
    async fn enroll_happy_path_persists_secret_to_file() {
        use httpmock::prelude::*;

        // SAFETY: test-only env var mutation; test is single-threaded (tokio::test).
        unsafe { std::env::set_var("ZREMOTE_ALLOW_FILE_KEY_FALLBACK", "1") };

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/api/enroll");
            then.status(201)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "agent_id": uuid::Uuid::new_v4().to_string(),
                    "session_token": "tok123",
                }));
        });

        let dir = tempdir().unwrap();
        let key_path = dir.path().join("agent.key");

        let result = run_enroll("test-code", &server.base_url(), Some(key_path.clone())).await;

        // SAFETY: same thread
        unsafe { std::env::remove_var("ZREMOTE_ALLOW_FILE_KEY_FALLBACK") };

        result.expect("enroll should succeed");
        mock.assert();

        assert!(key_path.exists(), "key file must be created");
        let meta = std::fs::metadata(&key_path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file must be mode 0600, got {mode:o}");
    }

    /// Enroll returns a redirect-as-error when server responds with 3xx.
    #[tokio::test]
    async fn enroll_redirect_returns_network_error() {
        use httpmock::prelude::*;

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/api/enroll");
            then.status(302)
                .header("location", "https://attacker.example.com/collect");
        });

        let dir = tempdir().unwrap();
        let key_path = dir.path().join("agent.key");
        let result = run_enroll("code", &server.base_url(), Some(key_path)).await;
        // redirect=none means 302 is treated as a non-success response
        assert!(result.is_err(), "3xx must not be silently followed");
    }

    /// If the server returns a non-2xx, run_enroll must return an error.
    #[tokio::test]
    async fn enroll_server_rejection_returns_error() {
        use httpmock::prelude::*;

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/api/enroll");
            then.status(400)
                .json_body(serde_json::json!({ "error": "enrollment_failed" }));
        });

        let dir = tempdir().unwrap();
        let key_path = dir.path().join("agent.key");
        let result = run_enroll("bad-code", &server.base_url(), Some(key_path)).await;
        assert!(result.is_err(), "should return error on rejection");
    }
}
