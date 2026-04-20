//! Agent ↔ server authentication + enrollment messages (RFC auth-overhaul §3).
//!
//! All messages are additive on top of the existing `Register` path used by
//! the legacy single-token flow. A v2 agent sends these; a v1 agent still
//! sends `Register`. See `AGENT_PROTOCOL_VERSION`.
//!
//! Messages containing secret bytes (`agent_secret`, `reconnect_token`,
//! `mac`, `new_secret`) use a manual `Debug` impl that redacts the secret
//! value so the byte contents never leak into tracing output.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Current agent-↔-server protocol version.
///
/// Bumped from 1 → 2 for the auth overhaul. See RFC §8 (Protocol Versioning).
pub const AGENT_PROTOCOL_VERSION: u32 = 2;

/// Agent → server messages for enrollment + per-connection auth.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum AgentAuthMessage {
    /// First-time enrollment, exchanging a one-shot code for a durable
    /// `(agent_id, agent_secret)` pair.
    Enroll {
        code: String,
        hostname: String,
        host_fingerprint: String,
        agent_version: String,
        os: String,
        arch: String,
    },
    /// Begin the HMAC challenge-response on a fresh connection.
    AuthHello {
        agent_id: String,
        protocol_version: u32,
        client_nonce: Vec<u8>,
    },
    /// Proof of possession of `agent_secret`.
    /// `mac = HMAC-SHA256(agent_secret, b"zremote-agent-auth-v1" || server_nonce || client_nonce || agent_id)`
    AuthResponse { mac: Vec<u8> },
    /// Fast-path reconnect using a short-lived server-issued token.
    Resume {
        session_id: String,
        reconnect_token: String,
    },
    /// Confirmation that the agent has persisted a rotated secret.
    /// `fingerprint = HMAC(new_secret, b"rotate-ack")`.
    RotateAck { fingerprint: Vec<u8> },
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
            } => f
                .debug_struct("Enroll")
                .field("code", &"<redacted>")
                .field("hostname", hostname)
                .field("host_fingerprint", host_fingerprint)
                .field("agent_version", agent_version)
                .field("os", os)
                .field("arch", arch)
                .finish(),
            Self::AuthHello {
                agent_id,
                protocol_version,
                client_nonce,
            } => f
                .debug_struct("AuthHello")
                .field("agent_id", agent_id)
                .field("protocol_version", protocol_version)
                .field("client_nonce_len", &client_nonce.len())
                .finish(),
            Self::AuthResponse { mac } => f
                .debug_struct("AuthResponse")
                .field("mac", &"<redacted>")
                .field("mac_len", &mac.len())
                .finish(),
            Self::Resume {
                session_id,
                reconnect_token: _,
            } => f
                .debug_struct("Resume")
                .field("session_id", session_id)
                .field("reconnect_token", &"<redacted>")
                .finish(),
            Self::RotateAck { fingerprint } => f
                .debug_struct("RotateAck")
                .field("fingerprint", &"<redacted>")
                .field("fingerprint_len", &fingerprint.len())
                .finish(),
        }
    }
}

/// Server → agent messages for enrollment + per-connection auth.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ServerAuthMessage {
    /// Enrollment succeeded; agent must persist `agent_id` + `agent_secret`.
    EnrollAck {
        agent_id: String,
        agent_secret: String,
        host_id: String,
    },
    EnrollReject {
        reason: EnrollRejectReason,
    },
    /// Sent after `AuthHello` to drive the HMAC challenge-response.
    AuthChallenge {
        server_nonce: Vec<u8>,
        ttl_secs: u32,
        server_time: DateTime<Utc>,
    },
    /// Auth succeeded. `reconnect_token` enables fast-path `Resume` on the
    /// next connection.
    AuthAccepted {
        session_id: String,
        reconnect_token: String,
    },
    AuthRejected {
        reason: AuthRejectReason,
    },
    /// Server-initiated secret rotation. Agent acks with `RotateAck`.
    RotateSecret {
        new_secret: String,
    },
}

impl std::fmt::Debug for ServerAuthMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnrollAck {
                agent_id,
                agent_secret: _,
                host_id,
            } => f
                .debug_struct("EnrollAck")
                .field("agent_id", agent_id)
                .field("agent_secret", &"<redacted>")
                .field("host_id", host_id)
                .finish(),
            Self::EnrollReject { reason } => f
                .debug_struct("EnrollReject")
                .field("reason", reason)
                .finish(),
            Self::AuthChallenge {
                server_nonce,
                ttl_secs,
                server_time,
            } => f
                .debug_struct("AuthChallenge")
                .field("server_nonce_len", &server_nonce.len())
                .field("ttl_secs", ttl_secs)
                .field("server_time", server_time)
                .finish(),
            Self::AuthAccepted {
                session_id,
                reconnect_token: _,
            } => f
                .debug_struct("AuthAccepted")
                .field("session_id", session_id)
                .field("reconnect_token", &"<redacted>")
                .finish(),
            Self::AuthRejected { reason } => f
                .debug_struct("AuthRejected")
                .field("reason", reason)
                .finish(),
            Self::RotateSecret { new_secret: _ } => f
                .debug_struct("RotateSecret")
                .field("new_secret", &"<redacted>")
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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthRejectReason {
    UnknownAgent,
    BadMac,
    ClockSkew,
    ProtocolVersion,
    Revoked,
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
    fn agent_messages_roundtrip() {
        roundtrip_agent(&AgentAuthMessage::Enroll {
            code: "ABCD-EFGH".into(),
            hostname: "myhost".into(),
            host_fingerprint: "fp-123".into(),
            agent_version: "0.14.2".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        });
        roundtrip_agent(&AgentAuthMessage::AuthHello {
            agent_id: "agent-1".into(),
            protocol_version: AGENT_PROTOCOL_VERSION,
            client_nonce: vec![1, 2, 3, 4],
        });
        roundtrip_agent(&AgentAuthMessage::AuthResponse {
            mac: vec![0x11; 32],
        });
        roundtrip_agent(&AgentAuthMessage::Resume {
            session_id: "sid".into(),
            reconnect_token: "token".into(),
        });
        roundtrip_agent(&AgentAuthMessage::RotateAck {
            fingerprint: vec![9, 9, 9, 9],
        });
    }

    #[test]
    fn server_messages_roundtrip() {
        roundtrip_server(&ServerAuthMessage::EnrollAck {
            agent_id: "a".into(),
            agent_secret: "s".into(),
            host_id: "h".into(),
        });
        roundtrip_server(&ServerAuthMessage::EnrollReject {
            reason: EnrollRejectReason::CodeExpired,
        });
        roundtrip_server(&ServerAuthMessage::AuthChallenge {
            server_nonce: vec![7; 16],
            ttl_secs: 30,
            server_time: chrono::Utc::now(),
        });
        roundtrip_server(&ServerAuthMessage::AuthAccepted {
            session_id: "sid".into(),
            reconnect_token: "tok".into(),
        });
        roundtrip_server(&ServerAuthMessage::AuthRejected {
            reason: AuthRejectReason::BadMac,
        });
        roundtrip_server(&ServerAuthMessage::RotateSecret {
            new_secret: "new".into(),
        });
    }

    #[test]
    fn reject_reasons_use_snake_case() {
        let json = serde_json::to_value(EnrollRejectReason::CodeAlreadyUsed).unwrap();
        assert_eq!(json, serde_json::json!("code_already_used"));

        let json = serde_json::to_value(AuthRejectReason::ProtocolVersion).unwrap();
        assert_eq!(json, serde_json::json!("protocol_version"));
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
        };
        let dbg = format!("{msg:?}");
        assert!(!dbg.contains("TOP-SECRET-CODE"));
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn debug_redacts_mac_bytes() {
        let msg = AgentAuthMessage::AuthResponse {
            mac: vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE],
        };
        let dbg = format!("{msg:?}");
        // The raw byte contents should never appear — only the length.
        assert!(!dbg.contains("AA"));
        assert!(!dbg.contains("170")); // 0xAA in decimal
        assert!(dbg.contains("<redacted>"));
        assert!(dbg.contains("mac_len"));
    }

    #[test]
    fn debug_redacts_enroll_ack_secret() {
        let msg = ServerAuthMessage::EnrollAck {
            agent_id: "a".into(),
            agent_secret: "VERY-SECRET-BEARER".into(),
            host_id: "h".into(),
        };
        let dbg = format!("{msg:?}");
        assert!(!dbg.contains("VERY-SECRET-BEARER"));
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn debug_redacts_rotate_secret() {
        let msg = ServerAuthMessage::RotateSecret {
            new_secret: "FRESH-SECRET".into(),
        };
        let dbg = format!("{msg:?}");
        assert!(!dbg.contains("FRESH-SECRET"));
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn debug_redacts_reconnect_token() {
        let msg = ServerAuthMessage::AuthAccepted {
            session_id: "sid".into(),
            reconnect_token: "RECON-XYZ".into(),
        };
        let dbg = format!("{msg:?}");
        assert!(!dbg.contains("RECON-XYZ"));
        assert!(dbg.contains("<redacted>"));

        let msg = AgentAuthMessage::Resume {
            session_id: "sid".into(),
            reconnect_token: "RECON-XYZ".into(),
        };
        let dbg = format!("{msg:?}");
        assert!(!dbg.contains("RECON-XYZ"));
    }

    #[test]
    fn agent_protocol_version_is_2() {
        assert_eq!(AGENT_PROTOCOL_VERSION, 2);
    }
}
