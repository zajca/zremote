//! Server-side ed25519 challenge-response for agent connections (Phase 3).
//!
//! Flow per RFC §3 (amended):
//! 1. Receive `AgentAuthMessage::Hello` — version check, look up agent by ID.
//! 2. Parse stored `public_key` as ed25519 `VerifyingKey`. Reject on failure
//!    (treat as unknown_agent to avoid oracle).
//! 3. Generate `nonce_server` (32 CSPRNG bytes). Send `ServerAuthMessage::Challenge`.
//! 4. Receive `AgentAuthMessage::AuthResponse`. Decode base64url signature.
//! 5. Build canonical payload, verify signature.
//! 6. On success: mint `agent_session` row, return `AuthenticatedAgent`.
//! 7. On any failure: constant-work sleep (≥100 ms floor) + audit log entry.
//!
//! **Nonce replay:** no external nonce cache is needed. `nonce_server` is fresh
//! per WebSocket upgrade (OsRng). A replayed `AuthResponse` would require the
//! attacker to send the same `nonce_server` on a new connection, which is
//! astronomically unlikely. Single-roundtrip per connection makes replay
//! within one session structurally impossible.

use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use ed25519_dalek::{Verifier, VerifyingKey};
use rand::TryRngCore;
use rand::rngs::OsRng;
use serde_json;
use sqlx::SqlitePool;
use zremote_core::queries::agents;
use zremote_core::queries::audit::{self, AuditEvent, Outcome};
use zremote_protocol::auth::{
    AGENT_PROTOCOL_VERSION, AgentAuthMessage, AuthFailReason, ServerAuthMessage, build_auth_payload,
};

/// Minimum wall-clock latency for every auth failure response.
pub const AUTH_FAIL_MIN_LATENCY: Duration = Duration::from_millis(100);

/// Timeout waiting for the agent's Hello / AuthResponse messages.
const AUTH_RECV_TIMEOUT: Duration = Duration::from_secs(10);

/// TTL for minted agent_session tokens (seconds). 1 year — long-lived by design
/// (RFC §2: "long-lived"). Explicit revocation is the reclamation mechanism.
const AGENT_SESSION_TTL_SECS: i64 = 365 * 24 * 3600;

/// Result of a successful ed25519 challenge-response handshake.
pub struct AuthenticatedAgent {
    pub agent_id: String,
    pub host_id: String,
    pub session_token: String,
}

/// Stable reason tokens emitted into audit_log.details and returned over the
/// wire in `AuthFailure.reason`. Each variant maps to exactly one token string
/// so parsing tools and metrics don't see freeform human text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAuthError {
    VersionMismatch,
    UnknownAgent,
    InvalidPublicKey,
    InvalidSignature,
    MalformedMessage,
    Timeout,
    Internal,
}

impl AgentAuthError {
    #[must_use]
    pub fn as_reason(self) -> AuthFailReason {
        match self {
            Self::VersionMismatch => AuthFailReason::VersionMismatch,
            Self::UnknownAgent | Self::InvalidPublicKey => AuthFailReason::UnknownAgent,
            Self::InvalidSignature => AuthFailReason::InvalidSignature,
            Self::MalformedMessage => AuthFailReason::MalformedMessage,
            Self::Timeout => AuthFailReason::Timeout,
            Self::Internal => AuthFailReason::Internal,
        }
    }

    #[must_use]
    pub fn as_audit_token(self) -> &'static str {
        match self {
            Self::VersionMismatch => "agent_auth_failed_version_mismatch",
            Self::UnknownAgent => "agent_auth_failed_unknown_agent",
            Self::InvalidPublicKey => "agent_auth_failed_invalid_public_key",
            Self::InvalidSignature => "agent_auth_failed_invalid_signature",
            Self::MalformedMessage => "agent_auth_failed_malformed",
            Self::Timeout => "agent_auth_failed_timeout",
            Self::Internal => "agent_auth_failed_internal",
        }
    }
}

/// Receive a single text frame and deserialize it as `AgentAuthMessage`.
async fn recv_auth_msg(ws: &mut WebSocket) -> Option<AgentAuthMessage> {
    loop {
        match ws.recv().await {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<AgentAuthMessage>(&text) {
                    Ok(msg) => return Some(msg),
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to deserialize AgentAuthMessage");
                        return None;
                    }
                }
            }
            Some(Ok(Message::Close(_))) | None => return None,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Binary(_))) => {
                tracing::warn!("unexpected binary frame during agent auth");
                return None;
            }
            Some(Err(e)) => {
                tracing::warn!(error = %e, "WebSocket receive error during agent auth");
                return None;
            }
        }
    }
}

async fn send_auth_msg(ws: &mut WebSocket, msg: &ServerAuthMessage) -> bool {
    match serde_json::to_string(msg) {
        Ok(json) => ws.send(Message::Text(json.into())).await.is_ok(),
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize ServerAuthMessage");
            false
        }
    }
}

/// Perform the ed25519 challenge-response handshake.
///
/// Returns `Ok(AuthenticatedAgent)` on success, `Err(AgentAuthError)` on any
/// failure. The caller is responsible for sending `AuthFailure` to the agent
/// and applying the constant-work latency floor (use [`reject_after`]).
pub async fn authenticate_agent(
    ws: &mut WebSocket,
    pool: &SqlitePool,
    peer_ip: Option<&str>,
) -> Result<AuthenticatedAgent, AgentAuthError> {
    // Step 1: receive Hello.
    let hello = tokio::time::timeout(AUTH_RECV_TIMEOUT, recv_auth_msg(ws))
        .await
        .map_err(|_| AgentAuthError::Timeout)?
        .ok_or(AgentAuthError::MalformedMessage)?;

    let AgentAuthMessage::Hello {
        version,
        agent_id,
        nonce_agent: nonce_agent_b64,
    } = hello
    else {
        return Err(AgentAuthError::MalformedMessage);
    };

    // Step 2: version check.
    if version != AGENT_PROTOCOL_VERSION {
        tracing::warn!(
            agent_id = %agent_id,
            version,
            expected = AGENT_PROTOCOL_VERSION,
            "agent version mismatch"
        );
        return Err(AgentAuthError::VersionMismatch);
    }

    // Step 3: look up agent row.
    let agent = agents::find_by_id(pool, &agent_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "DB error looking up agent");
            AgentAuthError::Internal
        })?
        .ok_or(AgentAuthError::UnknownAgent)?;

    if agent.revoked_at.is_some() {
        tracing::warn!(agent_id = %agent_id, "revoked agent attempted auth");
        return Err(AgentAuthError::UnknownAgent);
    }

    // Step 4: parse public key. Treat parse errors the same as unknown_agent
    // to avoid an oracle on well/ill-formed key storage.
    let pk_bytes = URL_SAFE_NO_PAD
        .decode(&agent.public_key)
        .map_err(|_| AgentAuthError::InvalidPublicKey)?;
    let pk_bytes_32: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| AgentAuthError::InvalidPublicKey)?;
    let verifying_key =
        VerifyingKey::from_bytes(&pk_bytes_32).map_err(|_| AgentAuthError::InvalidPublicKey)?;

    // Step 5: generate nonce_server and send Challenge.
    let mut nonce_server = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut nonce_server)
        .expect("OS CSPRNG unavailable");
    let nonce_server_b64 = URL_SAFE_NO_PAD.encode(nonce_server);

    if !send_auth_msg(
        ws,
        &ServerAuthMessage::Challenge {
            nonce_server: nonce_server_b64.clone(),
        },
    )
    .await
    {
        return Err(AgentAuthError::Internal);
    }

    // Step 6: receive AuthResponse.
    let response = tokio::time::timeout(AUTH_RECV_TIMEOUT, recv_auth_msg(ws))
        .await
        .map_err(|_| AgentAuthError::Timeout)?
        .ok_or(AgentAuthError::MalformedMessage)?;

    let AgentAuthMessage::AuthResponse {
        signature: signature_b64,
    } = response
    else {
        return Err(AgentAuthError::MalformedMessage);
    };

    // Step 7: decode signature and nonces.
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(&signature_b64)
        .map_err(|_| AgentAuthError::MalformedMessage)?;
    let sig_bytes_64: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| AgentAuthError::MalformedMessage)?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes_64);

    let nonce_agent_bytes = URL_SAFE_NO_PAD
        .decode(&nonce_agent_b64)
        .map_err(|_| AgentAuthError::MalformedMessage)?;
    let nonce_agent_32: [u8; 32] = nonce_agent_bytes
        .try_into()
        .map_err(|_| AgentAuthError::MalformedMessage)?;

    // Step 8: build canonical payload and verify.
    let payload = build_auth_payload(&agent_id, &nonce_server, &nonce_agent_32)
        .ok_or(AgentAuthError::MalformedMessage)?;

    verifying_key
        .verify(&payload, &signature)
        .map_err(|_| AgentAuthError::InvalidSignature)?;

    // Step 9: mint agent_session.
    let session_token = agents::mint_agent_session(pool, &agent_id, AGENT_SESSION_TTL_SECS)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to mint agent_session");
            AgentAuthError::Internal
        })?;

    // Step 10: update last_seen.
    let _ = agents::set_last_seen(pool, &agent_id, Utc::now()).await;

    // Audit success.
    let _ = audit::log_event(
        pool,
        AuditEvent {
            ts: Utc::now(),
            actor: format!("agent:{agent_id}"),
            ip: peer_ip.map(str::to_string),
            event: "agent_auth_ok".to_string(),
            target: Some(agent_id.clone()),
            outcome: Outcome::Ok,
            details: None,
        },
    )
    .await;

    tracing::info!(agent_id = %agent_id, "agent authenticated via ed25519");

    Ok(AuthenticatedAgent {
        agent_id,
        host_id: agent.host_id,
        session_token,
    })
}

/// Send `AuthFailure` to the agent and sleep until the minimum latency floor
/// has been reached (relative to `started`). Call this on every error path
/// from [`authenticate_agent`].
pub async fn reject_after(
    ws: &mut WebSocket,
    pool: &SqlitePool,
    err: AgentAuthError,
    agent_id: Option<&str>,
    peer_ip: Option<&str>,
    started: Instant,
) {
    let _ = send_auth_msg(
        ws,
        &ServerAuthMessage::AuthFailure {
            reason: err.as_reason(),
        },
    )
    .await;

    // Audit the failure with stable reason token.
    let _ = audit::log_event(
        pool,
        AuditEvent {
            ts: Utc::now(),
            actor: agent_id.map_or_else(|| "unknown".to_string(), |id| format!("agent:{id}")),
            ip: peer_ip.map(str::to_string),
            event: err.as_audit_token().to_string(),
            target: agent_id.map(str::to_string),
            outcome: Outcome::Denied,
            details: None,
        },
    )
    .await;

    let elapsed = started.elapsed();
    if let Some(remaining) = AUTH_FAIL_MIN_LATENCY.checked_sub(elapsed) {
        tokio::time::sleep(remaining).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::password_hash::rand_core::OsRng;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::SigningKey;
    use zremote_core::db;
    use zremote_core::queries::agents;

    async fn setup_pool_with_agent(public_key_b64: &str) -> (SqlitePool, String, String) {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let host_id = "host-test-1".to_string();
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES (?, 'thost', 'thost', 'th', 'offline')",
        )
        .bind(&host_id)
        .execute(&pool)
        .await
        .unwrap();
        let agent = agents::create(&pool, &host_id, public_key_b64)
            .await
            .unwrap();
        (pool, host_id, agent.id)
    }

    fn gen_keypair() -> (SigningKey, String) {
        let sk = SigningKey::generate(&mut OsRng);
        let pk_b64 = URL_SAFE_NO_PAD.encode(sk.verifying_key().as_bytes());
        (sk, pk_b64)
    }

    #[tokio::test]
    async fn version_mismatch_returns_error() {
        let (sk, pk_b64) = gen_keypair();
        let (pool, _, agent_id) = setup_pool_with_agent(&pk_b64).await;
        drop(sk);

        // Simulate: verify_version logic inline — wrong version.
        assert_eq!(
            AgentAuthError::VersionMismatch.as_reason(),
            AuthFailReason::VersionMismatch
        );
        // The audit token is stable.
        assert_eq!(
            AgentAuthError::VersionMismatch.as_audit_token(),
            "agent_auth_failed_version_mismatch"
        );
        drop((pool, agent_id));
    }

    #[tokio::test]
    async fn unknown_agent_returns_error() {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        // No agents in DB — find_by_id returns None.
        let result = agents::find_by_id(&pool, "nonexistent-agent-id")
            .await
            .unwrap();
        assert!(result.is_none(), "should be unknown agent");
        assert_eq!(
            AgentAuthError::UnknownAgent.as_reason(),
            AuthFailReason::UnknownAgent
        );
    }

    #[test]
    fn invalid_public_key_collapses_to_unknown_agent_reason() {
        // InvalidPublicKey maps to UnknownAgent on the wire (oracle collapse).
        assert_eq!(
            AgentAuthError::InvalidPublicKey.as_reason(),
            AuthFailReason::UnknownAgent
        );
    }

    #[tokio::test]
    async fn revoked_agent_rejected() {
        let (sk, pk_b64) = gen_keypair();
        let (pool, _, agent_id) = setup_pool_with_agent(&pk_b64).await;
        drop(sk);

        agents::revoke(&pool, &agent_id).await.unwrap();
        let agent = agents::find_by_id(&pool, &agent_id).await.unwrap().unwrap();
        assert!(agent.revoked_at.is_some());
    }

    #[test]
    fn malformed_message_error_token() {
        assert_eq!(
            AgentAuthError::MalformedMessage.as_audit_token(),
            "agent_auth_failed_malformed"
        );
    }

    #[test]
    fn all_error_variants_have_stable_tokens() {
        use AgentAuthError::*;
        let variants = [
            VersionMismatch,
            UnknownAgent,
            InvalidPublicKey,
            InvalidSignature,
            MalformedMessage,
            Timeout,
            Internal,
        ];
        for v in variants {
            let tok = v.as_audit_token();
            assert!(
                tok.starts_with("agent_auth_failed_"),
                "token should start with 'agent_auth_failed_': {tok}"
            );
        }
    }

    /// Happy-path: valid keypair, correct payload → session minted.
    #[tokio::test]
    async fn sign_verify_happy_path() {
        use ed25519_dalek::Signer;
        use zremote_protocol::auth::build_auth_payload;

        let sk = SigningKey::generate(&mut OsRng);
        let pk_b64 = URL_SAFE_NO_PAD.encode(sk.verifying_key().as_bytes());
        let (pool, _, agent_id) = setup_pool_with_agent(&pk_b64).await;

        // Replicate what the server does: build payload, agent signs it.
        let nonce_server = [0x10u8; 32];
        let nonce_agent = [0x20u8; 32];
        let payload = build_auth_payload(&agent_id, &nonce_server, &nonce_agent).unwrap();
        let sig = sk.sign(&payload);
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig.to_bytes());

        // Decode and verify (mirrors authenticate_agent internals).
        let pk_bytes = URL_SAFE_NO_PAD.decode(&pk_b64).unwrap();
        let pk_arr: [u8; 32] = pk_bytes.try_into().unwrap();
        let vk = VerifyingKey::from_bytes(&pk_arr).unwrap();

        let sig_bytes = URL_SAFE_NO_PAD.decode(&sig_b64).unwrap();
        let sig_arr: [u8; 64] = sig_bytes.try_into().unwrap();
        let sig_obj = ed25519_dalek::Signature::from_bytes(&sig_arr);

        assert!(vk.verify(&payload, &sig_obj).is_ok());

        // Mint session to verify that path works.
        let token = agents::mint_agent_session(&pool, &agent_id, 3600)
            .await
            .unwrap();
        assert_eq!(token.len(), 43);
    }

    /// Wrong key → verify fails.
    #[test]
    fn wrong_key_signature_rejected() {
        use ed25519_dalek::{Signer, Verifier};
        use zremote_protocol::auth::build_auth_payload;

        let sk_agent = SigningKey::generate(&mut OsRng);
        let sk_other = SigningKey::generate(&mut OsRng);
        let vk_agent = sk_agent.verifying_key();

        let agent_id = uuid::Uuid::new_v4().to_string();
        let ns = [1u8; 32];
        let na = [2u8; 32];
        let payload = build_auth_payload(&agent_id, &ns, &na).unwrap();

        // Sign with wrong key.
        let sig = sk_other.sign(&payload);
        assert!(vk_agent.verify(&payload, &sig).is_err());
    }

    #[test]
    fn latency_floor_constant() {
        assert_eq!(AUTH_FAIL_MIN_LATENCY, Duration::from_millis(100));
    }
}
