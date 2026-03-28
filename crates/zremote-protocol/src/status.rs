use std::fmt;

use serde::{Deserialize, Serialize};

/// Status of a terminal session.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Creating,
    Active,
    Suspended,
    Closed,
    Error,
    /// Forward-compatibility: unknown status values from newer servers.
    #[serde(other)]
    Unknown,
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Creating => write!(f, "creating"),
            Self::Active => write!(f, "active"),
            Self::Suspended => write!(f, "suspended"),
            Self::Closed => write!(f, "closed"),
            Self::Error => write!(f, "error"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl SessionStatus {
    /// Parse from a string (e.g. from database).
    /// Returns `Unknown` for unrecognized values.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "creating" => Self::Creating,
            "active" => Self::Active,
            "suspended" => Self::Suspended,
            "closed" => Self::Closed,
            "error" => Self::Error,
            _ => Self::Unknown,
        }
    }
}

/// Status of a host.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostStatus {
    Online,
    #[default]
    Offline,
    /// Forward-compatibility: unknown status values from newer servers.
    #[serde(other)]
    Unknown,
}

impl fmt::Display for HostStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Online => write!(f, "online"),
            Self::Offline => write!(f, "offline"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl HostStatus {
    /// Parse from a string (e.g. from database).
    /// Returns `Unknown` for unrecognized values.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "online" => Self::Online,
            "offline" => Self::Offline,
            _ => Self::Unknown,
        }
    }
}

/// Status of a knowledge base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeStatus {
    Ready,
    Indexing,
    Error,
    /// Forward-compatibility: unknown status values from newer servers.
    #[serde(other)]
    Unknown,
}

impl fmt::Display for KnowledgeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => write!(f, "ready"),
            Self::Indexing => write!(f, "indexing"),
            Self::Error => write!(f, "error"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl KnowledgeStatus {
    /// Parse from a string (e.g. from database).
    /// Returns `Unknown` for unrecognized values.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "ready" => Self::Ready,
            "indexing" => Self::Indexing,
            "error" => Self::Error,
            _ => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- SessionStatus tests ---

    #[test]
    fn session_status_serialization() {
        assert_eq!(
            serde_json::to_string(&SessionStatus::Creating).unwrap(),
            r#""creating""#
        );
        assert_eq!(
            serde_json::to_string(&SessionStatus::Active).unwrap(),
            r#""active""#
        );
        assert_eq!(
            serde_json::to_string(&SessionStatus::Suspended).unwrap(),
            r#""suspended""#
        );
        assert_eq!(
            serde_json::to_string(&SessionStatus::Closed).unwrap(),
            r#""closed""#
        );
    }

    #[test]
    fn session_status_deserialization() {
        assert_eq!(
            serde_json::from_str::<SessionStatus>(r#""creating""#).unwrap(),
            SessionStatus::Creating
        );
        assert_eq!(
            serde_json::from_str::<SessionStatus>(r#""active""#).unwrap(),
            SessionStatus::Active
        );
        assert_eq!(
            serde_json::from_str::<SessionStatus>(r#""suspended""#).unwrap(),
            SessionStatus::Suspended
        );
        assert_eq!(
            serde_json::from_str::<SessionStatus>(r#""closed""#).unwrap(),
            SessionStatus::Closed
        );
    }

    #[test]
    fn session_status_display() {
        assert_eq!(SessionStatus::Creating.to_string(), "creating");
        assert_eq!(SessionStatus::Active.to_string(), "active");
        assert_eq!(SessionStatus::Suspended.to_string(), "suspended");
        assert_eq!(SessionStatus::Closed.to_string(), "closed");
    }

    #[test]
    fn session_status_parse() {
        assert_eq!(SessionStatus::parse("creating"), SessionStatus::Creating);
        assert_eq!(SessionStatus::parse("active"), SessionStatus::Active);
        assert_eq!(SessionStatus::parse("suspended"), SessionStatus::Suspended);
        assert_eq!(SessionStatus::parse("closed"), SessionStatus::Closed);
        assert_eq!(SessionStatus::parse("error"), SessionStatus::Error);
        assert_eq!(
            SessionStatus::parse("something_new"),
            SessionStatus::Unknown
        );
    }

    #[test]
    fn session_status_unknown_deserialization() {
        // Forward compatibility: unknown values deserialize to Unknown
        assert_eq!(
            serde_json::from_str::<SessionStatus>(r#""future_status""#).unwrap(),
            SessionStatus::Unknown
        );
    }

    #[test]
    fn session_status_roundtrip() {
        for status in [
            SessionStatus::Creating,
            SessionStatus::Active,
            SessionStatus::Suspended,
            SessionStatus::Closed,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: SessionStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, parsed);
        }
    }

    // --- HostStatus tests ---

    #[test]
    fn host_status_serialization() {
        assert_eq!(
            serde_json::to_string(&HostStatus::Online).unwrap(),
            r#""online""#
        );
        assert_eq!(
            serde_json::to_string(&HostStatus::Offline).unwrap(),
            r#""offline""#
        );
    }

    #[test]
    fn host_status_deserialization() {
        assert_eq!(
            serde_json::from_str::<HostStatus>(r#""online""#).unwrap(),
            HostStatus::Online
        );
        assert_eq!(
            serde_json::from_str::<HostStatus>(r#""offline""#).unwrap(),
            HostStatus::Offline
        );
    }

    #[test]
    fn host_status_display() {
        assert_eq!(HostStatus::Online.to_string(), "online");
        assert_eq!(HostStatus::Offline.to_string(), "offline");
    }

    #[test]
    fn host_status_parse() {
        assert_eq!(HostStatus::parse("online"), HostStatus::Online);
        assert_eq!(HostStatus::parse("offline"), HostStatus::Offline);
        assert_eq!(HostStatus::parse("something_new"), HostStatus::Unknown);
    }

    #[test]
    fn host_status_unknown_deserialization() {
        assert_eq!(
            serde_json::from_str::<HostStatus>(r#""degraded""#).unwrap(),
            HostStatus::Unknown
        );
    }

    #[test]
    fn host_status_roundtrip() {
        for status in [HostStatus::Online, HostStatus::Offline] {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: HostStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, parsed);
        }
    }

    // --- KnowledgeStatus tests ---

    #[test]
    fn knowledge_status_serialization() {
        assert_eq!(
            serde_json::to_string(&KnowledgeStatus::Ready).unwrap(),
            r#""ready""#
        );
        assert_eq!(
            serde_json::to_string(&KnowledgeStatus::Indexing).unwrap(),
            r#""indexing""#
        );
        assert_eq!(
            serde_json::to_string(&KnowledgeStatus::Error).unwrap(),
            r#""error""#
        );
    }

    #[test]
    fn knowledge_status_deserialization() {
        assert_eq!(
            serde_json::from_str::<KnowledgeStatus>(r#""ready""#).unwrap(),
            KnowledgeStatus::Ready
        );
        assert_eq!(
            serde_json::from_str::<KnowledgeStatus>(r#""indexing""#).unwrap(),
            KnowledgeStatus::Indexing
        );
        assert_eq!(
            serde_json::from_str::<KnowledgeStatus>(r#""error""#).unwrap(),
            KnowledgeStatus::Error
        );
    }

    #[test]
    fn knowledge_status_display() {
        assert_eq!(KnowledgeStatus::Ready.to_string(), "ready");
        assert_eq!(KnowledgeStatus::Indexing.to_string(), "indexing");
        assert_eq!(KnowledgeStatus::Error.to_string(), "error");
    }

    #[test]
    fn knowledge_status_parse() {
        assert_eq!(KnowledgeStatus::parse("ready"), KnowledgeStatus::Ready);
        assert_eq!(
            KnowledgeStatus::parse("indexing"),
            KnowledgeStatus::Indexing
        );
        assert_eq!(KnowledgeStatus::parse("error"), KnowledgeStatus::Error);
        assert_eq!(
            KnowledgeStatus::parse("something_new"),
            KnowledgeStatus::Unknown
        );
    }

    #[test]
    fn knowledge_status_unknown_deserialization() {
        assert_eq!(
            serde_json::from_str::<KnowledgeStatus>(r#""processing""#).unwrap(),
            KnowledgeStatus::Unknown
        );
    }

    #[test]
    fn knowledge_status_roundtrip() {
        for status in [
            KnowledgeStatus::Ready,
            KnowledgeStatus::Indexing,
            KnowledgeStatus::Error,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: KnowledgeStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, parsed);
        }
    }
}
