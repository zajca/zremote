//! Agent ↔ server authentication + enrollment messages (RFC auth-overhaul §3,
//! amended Phase 3).
//!
//! **Phase 3 amendment:** agent runtime auth uses ed25519 signature
//! challenge-response rather than HMAC-SHA256. The RFC §3 draft described
//! HMAC with `agent_secret` stored argon2id-hashed, which is incompatible:
//! argon2id is one-way, so the server cannot re-derive the secret to verify
//! the MAC. ed25519 eliminates the asymmetry — the server stores only the
//! public key (safe at rest) and verifies signatures without needing any
//! secret material.
//!
//! **Wire protocol version:** `AGENT_PROTOCOL_VERSION = 2` (bumped in Phase 1,
//! unchanged here — the version covers the whole auth-overhaul family).
//!
//! **Signed payload (canonical form):**
//! ```text
//! b"zremote-agent-auth-v1"  (21 bytes, domain tag)
//! || agent_uuid_bytes        (16 bytes, UUID parsed from agent_id)
//! || nonce_server_bytes      (32 bytes, decoded from base64url)
//! || nonce_agent_bytes       (32 bytes, decoded from base64url)
//! ```
//! Total: 101 bytes, fully fixed-width. No length-confusion attack surface.
//!
//! **Legacy path:** the old `Register { token }` variant (`AgentMessage`) is
//! kept alive in `zremote-protocol/src/lib.rs` for one release cycle (RFC §9).
//! Once that window closes, the `lifecycle.rs` dispatch shim removes the
//! legacy branch and the HMAC types here are deleted.
//!
//! Messages containing secret bytes use a manual `Debug` impl that redacts the
//! value so byte contents never leak into tracing output.

use serde::{Deserialize, Serialize};

/// Current agent-↔-server protocol version.
///
/// Bumped from 1 → 2 for the auth overhaul (RFC §8).
pub const AGENT_PROTOCOL_VERSION: u32 = 2;

/// Domain separation tag prepended to every signed payload.
pub const AUTH_PAYLOAD_TAG: &[u8] = b"zremote-agent-auth-v1";

/// Build the canonical 101-byte payload that the agent signs and the server
/// verifies. Both sides must compute this identically:
///
/// `AUTH_PAYLOAD_TAG (21) || agent_uuid_bytes (16) || nonce_server (32) || nonce_agent (32)`
///
/// `agent_uuid_bytes` is the UUID parsed to its 16-byte binary representation
/// (avoids length-confusion attacks from variable-length string encoding).
/// Returns `None` if `agent_id` is not a valid UUID string.
#[must_use]
pub fn build_auth_payload(
    agent_id: &str,
    nonce_server: &[u8; 32],
    nonce_agent: &[u8; 32],
) -> Option<[u8; 101]> {
    let uuid = uuid::Uuid::parse_str(agent_id).ok()?;
    let uuid_bytes = *uuid.as_bytes();
    let mut payload = [0u8; 101];
    payload[..21].copy_from_slice(AUTH_PAYLOAD_TAG);
    payload[21..37].copy_from_slice(&uuid_bytes);
    payload[37..69].copy_from_slice(nonce_server);
    payload[69..101].copy_from_slice(nonce_agent);
    Some(payload)
}

/// Agent → server messages for enrollment + per-connection auth.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum AgentAuthMessage {
    /// First-time enrollment, exchanging a one-shot code for a durable
    /// `(agent_id, public_key)` pair. The enrollment code is a one-time
    /// server-issued token; the `public_key` is the agent's ed25519 verifying
    /// key (base64url, 32 bytes) whose matching signing key never leaves the
    /// agent host.
    Enroll {
        code: String,
        hostname: String,
        host_fingerprint: String,
        agent_version: String,
        os: String,
        arch: String,
        /// ed25519 verifying key (base64url, 32 bytes).
        public_key: String,
    },
    /// First message from agent on a fresh connection: announces identity
    /// and a fresh agent-generated nonce.
    Hello {
        version: u32,
        agent_id: String,
        /// base64url-encoded 32 random bytes.
        nonce_agent: String,
    },
    /// Proof of possession of the ed25519 signing key.
    /// `signature = Sign(signing_key, build_auth_payload(agent_id, nonce_server, nonce_agent))`
    AuthResponse {
        /// base64url-encoded ed25519 signature (64 bytes).
        signature: String,
    },
    /// Fast-path reconnect using a short-lived server-issued token.
    Resume {
        session_id: String,
        reconnect_token: String,
    },
    /// Confirmation that the agent has persisted a rotated public key.
    RotateAck { agent_id: String },
}

impl std::fmt::Debug for AgentAuthMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enroll {
                code: _,
                hostname,
                host_fingerprint,
                agent_version,
                os,
                arch,
                public_key,
            } => f
                .debug_struct("Enroll")
                .field("code", &"<redacted>")
                .field("hostname", hostname)
                .field("host_fingerprint", host_fingerprint)
                .field("agent_version", agent_version)
                .field("os", os)
                .field("arch", arch)
                .field("public_key_len", &public_key.len())
                .finish(),
            Self::Hello {
                version,
                agent_id,
                nonce_agent,
            } => f
                .debug_struct("Hello")
                .field("version", version)
                .field("agent_id", agent_id)
                .field("nonce_agent_len", &nonce_agent.len())
                .finish(),
            Self::AuthResponse { signature } => f
                .debug_struct("AuthResponse")
                .field("signature_len", &signature.len())
                .finish(),
            Self::Resume {
                session_id,
                reconnect_token: _,
            } => f
                .debug_struct("Resume")
                .field("session_id", session_id)
                .field("reconnect_token", &"<redacted>")
                .finish(),
            Self::RotateAck { agent_id } => f
                .debug_struct("RotateAck")
                .field("agent_id", agent_id)
                .finish(),
        }
    }
}

/// Server → agent messages for enrollment + per-connection auth.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ServerAuthMessage {
    /// Enrollment succeeded; agent must persist `agent_id` + the ed25519
    /// signing key it generated. The server stores only the public key.
    EnrollAck {
        agent_id: String,
        host_id: String,
    },
    EnrollReject {
        reason: EnrollRejectReason,
    },
    /// Sent after `Hello` to drive the ed25519 challenge-response.
    Challenge {
        /// base64url-encoded 32 random bytes.
        nonce_server: String,
    },
    /// Auth succeeded.
    AuthSuccess {
        session_id: String,
        reconnect_token: String,
    },
    /// Auth rejected. Identical payload for `unknown_agent` and
    /// `invalid_signature` — the server only distinguishes them in audit logs.
    AuthFailure {
        /// Stable reason token, not human text.
        reason: AuthFailReason,
    },
    /// Server-initiated public-key rotation.
    RotateKey {
        new_public_key: String,
    },
}

impl std::fmt::Debug for ServerAuthMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnrollAck { agent_id, host_id } => f
                .debug_struct("EnrollAck")
                .field("agent_id", agent_id)
                .field("host_id", host_id)
                .finish(),
            Self::EnrollReject { reason } => f
                .debug_struct("EnrollReject")
                .field("reason", reason)
                .finish(),
            Self::Challenge { nonce_server } => f
                .debug_struct("Challenge")
                .field("nonce_server_len", &nonce_server.len())
                .finish(),
            Self::AuthSuccess {
                session_id,
                reconnect_token: _,
            } => f
                .debug_struct("AuthSuccess")
                .field("session_id", session_id)
                .field("reconnect_token", &"<redacted>")
                .finish(),
            Self::AuthFailure { reason } => f
                .debug_struct("AuthFailure")
                .field("reason", reason)
                .finish(),
            Self::RotateKey { new_public_key } => f
                .debug_struct("RotateKey")
                .field("new_public_key_len", &new_public_key.len())
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnrollRejectReason {
    CodeExpired,
    CodeAlreadyUsed,
    CodeUnknown,
    RateLimited,
    InvalidPublicKey,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthFailReason {
    VersionMismatch,
    UnknownAgent,
    InvalidPublicKey,
    InvalidSignature,
    MalformedMessage,
    Timeout,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_agent(msg: &AgentAuthMessage) {
        let json = serde_json::to_value(msg).expect("serialize");
        let back: AgentAuthMessage = serde_json::from_value(json).expect("deserialize");
        assert_eq!(&back, msg);
    }

    fn roundtrip_server(msg: &ServerAuthMessage) {
        let json = serde_json::to_value(msg).expect("serialize");
        let back: ServerAuthMessage = serde_json::from_value(json).expect("deserialize");
        assert_eq!(&back, msg);
    }

    #[test]
    fn agent_protocol_version_is_2() {
        assert_eq!(AGENT_PROTOCOL_VERSION, 2);
    }

    #[test]
    fn agent_messages_roundtrip() {
        roundtrip_agent(&AgentAuthMessage::Enroll {
            code: "ABCD-EFGH".into(),
            hostname: "myhost".into(),
            host_fingerprint: "fp-123".into(),
            agent_version: "0.14.2".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            public_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        });
        roundtrip_agent(&AgentAuthMessage::Hello {
            version: AGENT_PROTOCOL_VERSION,
            agent_id: uuid::Uuid::new_v4().to_string(),
            nonce_agent: "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".into(),
        });
        roundtrip_agent(&AgentAuthMessage::AuthResponse {
            signature: "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC".into(),
        });
        roundtrip_agent(&AgentAuthMessage::Resume {
            session_id: "sid".into(),
            reconnect_token: "token".into(),
        });
        roundtrip_agent(&AgentAuthMessage::RotateAck {
            agent_id: "agent-1".into(),
        });
    }

    #[test]
    fn server_messages_roundtrip() {
        roundtrip_server(&ServerAuthMessage::EnrollAck {
            agent_id: "a".into(),
            host_id: "h".into(),
        });
        roundtrip_server(&ServerAuthMessage::EnrollReject {
            reason: EnrollRejectReason::CodeExpired,
        });
        roundtrip_server(&ServerAuthMessage::Challenge {
            nonce_server: "DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD".into(),
        });
        roundtrip_server(&ServerAuthMessage::AuthSuccess {
            session_id: "sid".into(),
            reconnect_token: "tok".into(),
        });
        roundtrip_server(&ServerAuthMessage::AuthFailure {
            reason: AuthFailReason::InvalidSignature,
        });
        roundtrip_server(&ServerAuthMessage::RotateKey {
            new_public_key: "EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE".into(),
        });
    }

    #[test]
    fn reject_reasons_use_snake_case() {
        let json = serde_json::to_value(EnrollRejectReason::CodeAlreadyUsed).unwrap();
        assert_eq!(json, serde_json::json!("code_already_used"));

        let json = serde_json::to_value(AuthFailReason::VersionMismatch).unwrap();
        assert_eq!(json, serde_json::json!("version_mismatch"));

        let json = serde_json::to_value(AuthFailReason::InvalidSignature).unwrap();
        assert_eq!(json, serde_json::json!("invalid_signature"));
    }

    #[test]
    fn debug_redacts_enroll_code() {
        let msg = AgentAuthMessage::Enroll {
            code: "TOP-SECRET-CODE".into(),
            hostname: "h".into(),
            host_fingerprint: "fp".into(),
            agent_version: "v".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            public_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        };
        let dbg = format!("{msg:?}");
        assert!(!dbg.contains("TOP-SECRET-CODE"));
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn debug_redacts_reconnect_token() {
        let msg = AgentAuthMessage::Resume {
            session_id: "sid".into(),
            reconnect_token: "RECON-XYZ".into(),
        };
        let dbg = format!("{msg:?}");
        assert!(!dbg.contains("RECON-XYZ"));
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn debug_redacts_auth_success_reconnect_token() {
        let msg = ServerAuthMessage::AuthSuccess {
            session_id: "sid".into(),
            reconnect_token: "SECRET-TOKEN".into(),
        };
        let dbg = format!("{msg:?}");
        assert!(!dbg.contains("SECRET-TOKEN"));
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn build_auth_payload_deterministic() {
        let agent_id = "550e8400-e29b-41d4-a716-446655440000";
        let ns = [1u8; 32];
        let na = [2u8; 32];
        let p1 = build_auth_payload(agent_id, &ns, &na).unwrap();
        let p2 = build_auth_payload(agent_id, &ns, &na).unwrap();
        assert_eq!(p1, p2);
        assert_eq!(p1.len(), 101);
        // Domain tag
        assert_eq!(&p1[..21], AUTH_PAYLOAD_TAG);
        // UUID bytes (known from the test UUID)
        assert_eq!(
            &p1[21..37],
            &[
                0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44,
                0x00, 0x00
            ]
        );
        // Nonces
        assert_eq!(&p1[37..69], &[1u8; 32]);
        assert_eq!(&p1[69..101], &[2u8; 32]);
    }

    #[test]
    fn build_auth_payload_rejects_invalid_uuid() {
        let ns = [1u8; 32];
        let na = [2u8; 32];
        assert!(build_auth_payload("not-a-uuid", &ns, &na).is_none());
        assert!(build_auth_payload("", &ns, &na).is_none());
    }

    #[test]
    fn build_auth_payload_different_nonces_produce_different_payload() {
        let agent_id = "550e8400-e29b-41d4-a716-446655440000";
        let ns1 = [1u8; 32];
        let ns2 = [9u8; 32];
        let na = [2u8; 32];
        let p1 = build_auth_payload(agent_id, &ns1, &na).unwrap();
        let p2 = build_auth_payload(agent_id, &ns2, &na).unwrap();
        assert_ne!(p1, p2);
    }

    /// Sign → verify roundtrip with a freshly generated keypair.
    #[cfg(test)]
    #[test]
    fn sign_verify_roundtrip() {
        use ed25519_dalek::{Signer, SigningKey, Verifier};
        use rand_core::OsRng;

        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        let agent_id = uuid::Uuid::new_v4().to_string();
        let nonce_server = [7u8; 32];
        let nonce_agent = [3u8; 32];

        let payload = build_auth_payload(&agent_id, &nonce_server, &nonce_agent).unwrap();
        let signature = signing_key.sign(&payload);

        assert!(verifying_key.verify(&payload, &signature).is_ok());
    }

    /// Tampered signature (flip one bit in the middle) must be rejected.
    #[test]
    fn tampered_signature_rejected() {
        use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
        use rand_core::OsRng;

        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        let agent_id = uuid::Uuid::new_v4().to_string();
        let ns = [5u8; 32];
        let na = [6u8; 32];
        let payload = build_auth_payload(&agent_id, &ns, &na).unwrap();
        let signature = signing_key.sign(&payload);

        // Flip one byte in the middle of the signature.
        let mut sig_bytes: [u8; 64] = signature.to_bytes();
        sig_bytes[32] ^= 0xFF;
        let bad_sig = Signature::from_bytes(&sig_bytes);

        assert!(verifying_key.verify(&payload, &bad_sig).is_err());
    }

    /// Signature from the right key but wrong `agent_id` in the payload must fail.
    #[test]
    fn wrong_agent_id_payload_rejected() {
        use ed25519_dalek::{Signer, SigningKey, Verifier};
        use rand_core::OsRng;

        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        let real_agent_id = uuid::Uuid::new_v4().to_string();
        let other_agent_id = uuid::Uuid::new_v4().to_string();
        let ns = [8u8; 32];
        let na = [9u8; 32];

        // Sign payload for real_agent_id.
        let payload = build_auth_payload(&real_agent_id, &ns, &na).unwrap();
        let signature = signing_key.sign(&payload);

        // Verify against payload with other_agent_id — must fail.
        let wrong_payload = build_auth_payload(&other_agent_id, &ns, &na).unwrap();
        assert!(verifying_key.verify(&wrong_payload, &signature).is_err());
    }
}
