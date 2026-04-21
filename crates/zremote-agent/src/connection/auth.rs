//! Client-side ed25519 challenge-response (Phase 3).
//!
//! Mirrors server `auth/agent_auth.rs`. Flow:
//! 1. Send `AgentAuthMessage::Hello { version, agent_id, nonce_agent }`.
//! 2. Receive `ServerAuthMessage::Challenge { nonce_server }`.
//! 3. Build canonical payload, sign with ed25519 signing key.
//! 4. Send `AgentAuthMessage::AuthResponse { signature }`.
//! 5. Receive `ServerAuthMessage::AuthSuccess { session_id, reconnect_token }`.
//!
//! On `AuthFailure { reason: Revoked }`, exits with code 1 (no retry).

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer, SigningKey};
use futures_util::StreamExt;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use zremote_protocol::auth::{
    AGENT_PROTOCOL_VERSION, AgentAuthMessage, AuthFailReason, ServerAuthMessage, build_auth_payload,
};

use super::{ConnectionError, WsStream};

const AUTH_TIMEOUT: Duration = Duration::from_secs(15);

/// Error type for auth-specific failures.
#[derive(Debug)]
pub enum AuthError {
    Connection(ConnectionError),
    /// Server rejected us — this agent's credentials are revoked.
    /// The caller should not retry; log and exit.
    Revoked,
    /// Server rejected us for another reason (wrong key, malformed, etc.).
    Rejected(AuthFailReason),
    /// Handshake timed out.
    Timeout,
    /// Payload could not be built (agent_id is not a valid UUID).
    InvalidAgentId,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connection(e) => write!(f, "connection error during auth: {e}"),
            Self::Revoked => write!(f, "agent credentials revoked — re-enroll required"),
            Self::Rejected(r) => write!(f, "auth rejected by server: {r:?}"),
            Self::Timeout => write!(f, "auth handshake timed out"),
            Self::InvalidAgentId => write!(f, "agent_id is not a valid UUID"),
        }
    }
}

impl std::error::Error for AuthError {}

impl From<ConnectionError> for AuthError {
    fn from(e: ConnectionError) -> Self {
        Self::Connection(e)
    }
}

/// Result of a successful auth handshake.
pub struct AuthSuccess {
    pub session_id: String,
    pub reconnect_token: String,
}

/// Perform the ed25519 challenge-response handshake on an already-open WS stream.
/// Returns the `AuthSuccess` on success; on `Revoked`, returns `AuthError::Revoked`
/// so the caller can exit 1 without retry.
pub async fn authenticate(
    ws: &mut WsStream,
    agent_id: &str,
    signing_key: &SigningKey,
) -> Result<AuthSuccess, AuthError> {
    // Generate a fresh 32-byte agent nonce.
    use rand::TryRngCore;
    let mut nonce_agent = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut nonce_agent)
        .expect("OS CSPRNG must be available");
    let nonce_agent_b64 = URL_SAFE_NO_PAD.encode(nonce_agent);

    // Step 1: send Hello.
    let hello = AgentAuthMessage::Hello {
        version: AGENT_PROTOCOL_VERSION,
        agent_id: agent_id.to_string(),
        nonce_agent: nonce_agent_b64.clone(),
    };
    send_auth_msg(ws, &hello).await?;

    // Step 2: receive Challenge.
    let challenge = recv_auth_msg(ws).await?;
    let ServerAuthMessage::Challenge {
        nonce_server: ns_b64,
    } = challenge
    else {
        return Err(ConnectionError::UnexpectedRegisterResponse(format!(
            "expected Challenge, got {challenge:?}"
        ))
        .into());
    };

    let ns_bytes = URL_SAFE_NO_PAD.decode(&ns_b64).map_err(|_| {
        ConnectionError::UnexpectedRegisterResponse("bad nonce_server base64".into())
    })?;
    let ns_arr: [u8; 32] = ns_bytes.try_into().map_err(|_| {
        ConnectionError::UnexpectedRegisterResponse("nonce_server wrong length".into())
    })?;

    // Step 3: build canonical payload and sign.
    let payload =
        build_auth_payload(agent_id, &ns_arr, &nonce_agent).ok_or(AuthError::InvalidAgentId)?;
    let sig = signing_key.sign(&payload);
    let sig_b64 = URL_SAFE_NO_PAD.encode(sig.to_bytes());

    // Step 4: send AuthResponse.
    let resp = AgentAuthMessage::AuthResponse { signature: sig_b64 };
    send_auth_msg(ws, &resp).await?;

    // Step 5: receive AuthSuccess or AuthFailure.
    let result = recv_auth_msg(ws).await?;
    match result {
        ServerAuthMessage::AuthSuccess {
            session_id,
            reconnect_token,
        } => Ok(AuthSuccess {
            session_id,
            reconnect_token,
        }),
        ServerAuthMessage::AuthFailure { reason } => {
            if matches!(reason, AuthFailReason::UnknownAgent) {
                // UnknownAgent covers both revoked and truly unknown. We log
                // and signal the caller to exit without retry.
                Err(AuthError::Revoked)
            } else {
                Err(AuthError::Rejected(reason))
            }
        }
        other => Err(ConnectionError::UnexpectedRegisterResponse(format!(
            "expected AuthSuccess or AuthFailure, got {other:?}"
        ))
        .into()),
    }
}

/// Send a single `AgentAuthMessage` as a JSON text frame.
async fn send_auth_msg(ws: &mut WsStream, msg: &AgentAuthMessage) -> Result<(), AuthError> {
    use futures_util::SinkExt;
    let json = serde_json::to_string(msg).map_err(ConnectionError::Serialize)?;
    ws.send(Message::Text(json.into()))
        .await
        .map_err(ConnectionError::Send)?;
    Ok(())
}

/// Receive a single `ServerAuthMessage` from the WS stream with timeout.
async fn recv_auth_msg(ws: &mut WsStream) -> Result<ServerAuthMessage, AuthError> {
    timeout(AUTH_TIMEOUT, recv_auth_msg_inner(ws))
        .await
        .map_err(|_| AuthError::Timeout)?
}

async fn recv_auth_msg_inner(ws: &mut WsStream) -> Result<ServerAuthMessage, AuthError> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str::<ServerAuthMessage>(&text)
                    .map_err(ConnectionError::Deserialize)
                    .map_err(AuthError::Connection);
            }
            Some(Ok(Message::Close(_))) | None => {
                return Err(ConnectionError::ConnectionClosed.into());
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Binary(_) | Message::Frame(_))) => {
                return Err(ConnectionError::UnexpectedRegisterResponse(
                    "unexpected binary frame during auth".into(),
                )
                .into());
            }
            Some(Err(e)) => return Err(ConnectionError::Receive(e).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey as DalekKey;

    #[test]
    fn auth_challenge_response_round_trip() {
        // Verify the signing/verification logic in isolation (no network).
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use ed25519_dalek::Verifier;
        use ed25519_dalek::ed25519::signature::rand_core::OsRng;
        use zremote_protocol::auth::build_auth_payload;

        let sk = DalekKey::generate(&mut OsRng);
        let vk = sk.verifying_key();

        let agent_id = uuid::Uuid::new_v4().to_string();
        let nonce_server = [0xABu8; 32];
        let nonce_agent = [0xCDu8; 32];

        let payload = build_auth_payload(&agent_id, &nonce_server, &nonce_agent).unwrap();
        let sig = sk.sign(&payload);
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig.to_bytes());

        // Verify round-trip through base64.
        let sig_bytes = URL_SAFE_NO_PAD.decode(&sig_b64).unwrap();
        let sig_arr: [u8; 64] = sig_bytes.try_into().unwrap();
        let sig_obj = ed25519_dalek::Signature::from_bytes(&sig_arr);
        assert!(vk.verify(&payload, &sig_obj).is_ok());
    }

    #[test]
    fn wrong_key_fails_verification() {
        use ed25519_dalek::Verifier;
        use ed25519_dalek::ed25519::signature::rand_core::OsRng;
        use zremote_protocol::auth::build_auth_payload;

        let sk_agent = DalekKey::generate(&mut OsRng);
        let sk_other = DalekKey::generate(&mut OsRng);
        let vk_agent = sk_agent.verifying_key();

        let agent_id = uuid::Uuid::new_v4().to_string();
        let ns = [1u8; 32];
        let na = [2u8; 32];
        let payload = build_auth_payload(&agent_id, &ns, &na).unwrap();

        let sig = sk_other.sign(&payload);
        assert!(vk_agent.verify(&payload, &sig).is_err());
    }
}
