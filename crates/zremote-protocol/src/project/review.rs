use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::DiffSource;

/// Which side of a diff a comment is anchored to. Mirrors the GitHub PR
/// review comment API (`left` = pre-image, `right` = post-image).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReviewSide {
    /// Pre-image (deleted / old line).
    Left,
    /// Post-image (added / context / new line).
    Right,
}

/// A single inline review comment. Schema mirrors GitHub's PR review comment
/// API (Gitea / Forgejo / GitLab all mimic it), so a future "import PR
/// comments" / "export review to GitHub" feature is a field rename only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewComment {
    /// Local UUID (GitHub returns i64; ours is a Uuid for local uniqueness).
    pub id: Uuid,
    /// Relative path, forward slashes (matches `DiffFileSummary.path`).
    pub path: String,
    /// SHA the comment is anchored to. Required so a comment survives a
    /// reload of the same commit and future PR export has no custom mapping.
    pub commit_id: String,
    /// `left` = pre-image (removed/old), `right` = post-image
    /// (added/context/new). For a single-line comment this is the only side.
    pub side: ReviewSide,
    /// 1-based line on `side`. For a single-line comment, only `line` is set.
    /// For a multi-line comment, `line` is the end of the range.
    pub line: u32,
    /// Multi-line: side the range starts on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_side: Option<ReviewSide>,
    /// Multi-line: 1-based line the range starts on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    /// Markdown body of the comment.
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDelivery {
    /// Inject body into an existing agent session's PTY stdin.
    InjectSession,
    /// Start a new Claude task with the review as initial prompt.
    StartClaudeTask,
    /// (Future) send via MCP. Reserved so we don't break wire compat.
    McpTool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendReviewRequest {
    pub project_id: String,
    /// Diff source the review was drafted against. Echoed back in the
    /// response so the injected prompt can cite it.
    pub source: DiffSource,
    pub comments: Vec<ReviewComment>,
    pub delivery: ReviewDelivery,
    /// Required when `delivery == InjectSession`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    /// Optional freeform preamble (e.g. "Please address these comments:").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendReviewResponse {
    /// Session the review was routed to (new or existing).
    pub session_id: Uuid,
    /// Number of comments actually delivered.
    pub delivered: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).expect("serialize");
        let parsed: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*value, parsed);
    }

    #[test]
    fn review_side_roundtrip() {
        roundtrip(&ReviewSide::Left);
        roundtrip(&ReviewSide::Right);
    }

    #[test]
    fn review_side_json_is_lowercase() {
        assert_eq!(
            serde_json::to_string(&ReviewSide::Left).unwrap(),
            "\"left\""
        );
        assert_eq!(
            serde_json::to_string(&ReviewSide::Right).unwrap(),
            "\"right\""
        );
    }

    #[test]
    fn review_comment_single_line_roundtrip() {
        roundtrip(&ReviewComment {
            id: Uuid::new_v4(),
            path: "src/lib.rs".to_string(),
            commit_id: "abcdef0123456789".to_string(),
            side: ReviewSide::Right,
            line: 42,
            start_side: None,
            start_line: None,
            body: "use `tracing::info!` instead".to_string(),
            created_at: Utc::now(),
        });
    }

    #[test]
    fn review_comment_multi_line_roundtrip() {
        roundtrip(&ReviewComment {
            id: Uuid::new_v4(),
            path: "src/lib.rs".to_string(),
            commit_id: "abcdef0123456789".to_string(),
            side: ReviewSide::Right,
            line: 48,
            start_side: Some(ReviewSide::Right),
            start_line: Some(42),
            body: "this block could be a single `.map()`".to_string(),
            created_at: Utc::now(),
        });
    }

    #[test]
    fn review_comment_minimal_omits_optional_fields() {
        let comment = ReviewComment {
            id: Uuid::new_v4(),
            path: "a.rs".to_string(),
            commit_id: "abc".to_string(),
            side: ReviewSide::Right,
            line: 1,
            start_side: None,
            start_line: None,
            body: String::new(),
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&comment).unwrap();
        assert!(
            !json.contains("start_side"),
            "start_side should be skipped when None; json: {json}"
        );
        assert!(
            !json.contains("start_line"),
            "start_line should be skipped when None; json: {json}"
        );
    }

    #[test]
    fn review_comment_accepts_missing_optional_fields() {
        let json = r#"{
            "id":"550e8400-e29b-41d4-a716-446655440000",
            "path":"src/a.rs",
            "commit_id":"abc",
            "side":"right",
            "line":10,
            "body":"nit",
            "created_at":"2026-04-20T00:00:00Z"
        }"#;
        let parsed: ReviewComment = serde_json::from_str(json).expect("deserialize");
        assert!(parsed.start_side.is_none());
        assert!(parsed.start_line.is_none());
    }

    #[test]
    fn review_delivery_roundtrip() {
        for delivery in [
            ReviewDelivery::InjectSession,
            ReviewDelivery::StartClaudeTask,
            ReviewDelivery::McpTool,
        ] {
            roundtrip(&delivery);
        }
    }

    #[test]
    fn review_delivery_json_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&ReviewDelivery::InjectSession).unwrap(),
            "\"inject_session\""
        );
        assert_eq!(
            serde_json::to_string(&ReviewDelivery::StartClaudeTask).unwrap(),
            "\"start_claude_task\""
        );
        assert_eq!(
            serde_json::to_string(&ReviewDelivery::McpTool).unwrap(),
            "\"mcp_tool\""
        );
    }

    #[test]
    fn send_review_request_roundtrip() {
        roundtrip(&SendReviewRequest {
            project_id: "proj-1".to_string(),
            source: DiffSource::WorkingTree,
            comments: vec![ReviewComment {
                id: Uuid::new_v4(),
                path: "src/a.rs".to_string(),
                commit_id: "abc".to_string(),
                side: ReviewSide::Right,
                line: 12,
                start_side: None,
                start_line: None,
                body: "nit".to_string(),
                created_at: Utc::now(),
            }],
            delivery: ReviewDelivery::InjectSession,
            session_id: Some(Uuid::new_v4()),
            preamble: Some("Please address:".to_string()),
        });
        roundtrip(&SendReviewRequest {
            project_id: "proj-1".to_string(),
            source: DiffSource::Commit {
                sha: "abc".to_string(),
            },
            comments: vec![],
            delivery: ReviewDelivery::StartClaudeTask,
            session_id: None,
            preamble: None,
        });
    }

    #[test]
    fn send_review_request_accepts_missing_optional_fields() {
        let json = r#"{
            "project_id":"p",
            "source":{"kind":"working_tree"},
            "comments":[],
            "delivery":"inject_session"
        }"#;
        let parsed: SendReviewRequest = serde_json::from_str(json).expect("deserialize");
        assert!(parsed.session_id.is_none());
        assert!(parsed.preamble.is_none());
    }

    #[test]
    fn send_review_response_roundtrip() {
        roundtrip(&SendReviewResponse {
            session_id: Uuid::new_v4(),
            delivered: 5,
        });
    }
}
