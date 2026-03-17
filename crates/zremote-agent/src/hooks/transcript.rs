use std::collections::HashSet;

use serde::Deserialize;
use uuid::Uuid;
use zremote_protocol::TranscriptRole;

/// A parsed transcript entry ready to be sent as a protocol message.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    pub role: TranscriptRole,
    pub content: String,
    pub tool_call_id: Option<Uuid>,
}

/// Raw token usage data extracted from a transcript JSONL line.
#[derive(Debug, Clone, Default)]
pub struct TokenUsageData {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub model: Option<String>,
}

/// A content block in a Claude API message.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: Option<String>,
        name: Option<String>,
        input: Option<serde_json::Value>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: Option<String>,
        content: Option<serde_json::Value>,
    },
    #[serde(other)]
    Other,
}

/// The nested `message` object found in Claude Code's JSONL format.
///
/// Claude Code writes lines like:
/// ```json
/// {"type":"assistant","message":{"role":"assistant","content":[...],"usage":{...},"model":"..."},"requestId":"..."}
/// ```
#[derive(Debug, Deserialize)]
struct MessagePayload {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<serde_json::Value>,
    #[serde(default)]
    usage: Option<UsageBlock>,
    #[serde(default)]
    model: Option<String>,
}

/// A message entry in the transcript JSONL.
///
/// Supports both flat format (`{"role":"...", "usage":{...}}`) and
/// nested format (`{"type":"...", "message":{"role":"...", "usage":{...}}}`).
#[derive(Debug, Deserialize)]
struct TranscriptLine {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<serde_json::Value>,
    #[serde(default)]
    usage: Option<UsageBlock>,
    #[serde(default)]
    model: Option<String>,
    // type field to distinguish different line types
    #[serde(rename = "type", default)]
    line_type: Option<String>,
    /// Nested message object (Claude Code's actual format).
    #[serde(default)]
    message: Option<MessagePayload>,
    /// Request ID for deduplication (multiple lines per API turn share the same ID).
    #[serde(default, alias = "request_id")]
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    /// Task slug (e.g. "rename-myremote-to-zremote") present in Claude Code JSONL.
    #[serde(default)]
    slug: Option<String>,
}

impl TranscriptLine {
    fn role(&self) -> Option<&str> {
        self.message
            .as_ref()
            .and_then(|m| m.role.as_deref())
            .or(self.role.as_deref())
    }

    fn content(&self) -> Option<&serde_json::Value> {
        self.message
            .as_ref()
            .and_then(|m| m.content.as_ref())
            .or(self.content.as_ref())
    }

    fn usage(&self) -> Option<&UsageBlock> {
        self.message
            .as_ref()
            .and_then(|m| m.usage.as_ref())
            .or(self.usage.as_ref())
    }

    fn model(&self) -> Option<&str> {
        self.message
            .as_ref()
            .and_then(|m| m.model.as_deref())
            .or(self.model.as_deref())
    }
}

#[derive(Debug, Deserialize)]
struct UsageBlock {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

/// Parse a Claude Code transcript JSONL file from a given byte offset.
///
/// Returns:
/// - Vec of transcript entries (messages)
/// - New byte offset (for incremental parsing)
/// - Vec of token usage data (for metrics aggregation)
/// - Optional slug (first non-None slug found in the transcript)
pub async fn parse_transcript_file(
    path: &str,
    offset: u64,
) -> Result<
    (
        Vec<TranscriptEntry>,
        u64,
        Vec<TokenUsageData>,
        Option<String>,
    ),
    std::io::Error,
> {
    let data = tokio::fs::read(path).await?;
    let total_len = data.len() as u64;

    if offset >= total_len {
        return Ok((Vec::new(), total_len, Vec::new(), None));
    }

    let slice = &data[offset as usize..];
    let text = String::from_utf8_lossy(slice);

    let mut entries = Vec::new();
    let mut token_data = Vec::new();
    let mut seen_request_ids: HashSet<String> = HashSet::new();
    let mut slug_found: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(parsed) = serde_json::from_str::<TranscriptLine>(line) else {
            continue;
        };

        if slug_found.is_none() {
            slug_found = parsed.slug.clone();
        }

        // Extract token usage (deduplicate by requestId)
        if let Some(usage) = parsed.usage() {
            let is_duplicate = parsed
                .request_id
                .as_ref()
                .is_some_and(|id| !seen_request_ids.insert(id.clone()));

            if !is_duplicate {
                token_data.push(TokenUsageData {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cache_read_input_tokens: usage.cache_read_input_tokens,
                    cache_creation_input_tokens: usage.cache_creation_input_tokens,
                    model: parsed.model().map(String::from),
                });
            }
        }

        // Extract message content
        let role = match parsed.role() {
            Some("assistant") => TranscriptRole::Assistant,
            Some("user") => TranscriptRole::User,
            Some("system") => TranscriptRole::System,
            _ => continue,
        };

        if let Some(content) = parsed.content() {
            match content {
                serde_json::Value::String(s) => {
                    entries.push(TranscriptEntry {
                        role,
                        content: s.clone(),
                        tool_call_id: None,
                    });
                }
                serde_json::Value::Array(blocks) => {
                    for block in blocks {
                        if let Ok(cb) = serde_json::from_value::<ContentBlock>(block.clone()) {
                            match cb {
                                ContentBlock::Text { text } => {
                                    if !text.is_empty() {
                                        entries.push(TranscriptEntry {
                                            role,
                                            content: text,
                                            tool_call_id: None,
                                        });
                                    }
                                }
                                ContentBlock::ToolUse { id, name, input } => {
                                    let tool_call_id = id
                                        .as_deref()
                                        .map(|s| Uuid::new_v5(&Uuid::NAMESPACE_URL, s.as_bytes()));
                                    let desc = format!(
                                        "Tool: {} | Input: {}",
                                        name.as_deref().unwrap_or("unknown"),
                                        input
                                            .as_ref()
                                            .map(|v| {
                                                let s =
                                                    serde_json::to_string(v).unwrap_or_default();
                                                if s.len() > 200 {
                                                    format!("{}...", &s[..200])
                                                } else {
                                                    s
                                                }
                                            })
                                            .unwrap_or_default()
                                    );
                                    entries.push(TranscriptEntry {
                                        role: TranscriptRole::Tool,
                                        content: desc,
                                        tool_call_id,
                                    });
                                }
                                ContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                } => {
                                    let tool_call_id = tool_use_id
                                        .as_deref()
                                        .map(|s| Uuid::new_v5(&Uuid::NAMESPACE_URL, s.as_bytes()));
                                    let result_text = content
                                        .as_ref()
                                        .map(|v| {
                                            let s = serde_json::to_string(v).unwrap_or_default();
                                            if s.len() > 500 {
                                                format!("{}...", &s[..500])
                                            } else {
                                                s
                                            }
                                        })
                                        .unwrap_or_default();
                                    entries.push(TranscriptEntry {
                                        role: TranscriptRole::Tool,
                                        content: result_text,
                                        tool_call_id,
                                    });
                                }
                                ContentBlock::Other => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok((entries, total_len, token_data, slug_found))
}

/// Parse transcript content from a string (for testing).
pub fn parse_transcript_str(
    text: &str,
) -> (Vec<TranscriptEntry>, Vec<TokenUsageData>, Option<String>) {
    let mut entries = Vec::new();
    let mut token_data = Vec::new();
    let mut seen_request_ids: HashSet<String> = HashSet::new();
    let mut slug_found: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(parsed) = serde_json::from_str::<TranscriptLine>(line) else {
            continue;
        };

        if slug_found.is_none() {
            slug_found = parsed.slug.clone();
        }

        if let Some(usage) = parsed.usage() {
            let is_duplicate = parsed
                .request_id
                .as_ref()
                .is_some_and(|id| !seen_request_ids.insert(id.clone()));

            if !is_duplicate {
                token_data.push(TokenUsageData {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cache_read_input_tokens: usage.cache_read_input_tokens,
                    cache_creation_input_tokens: usage.cache_creation_input_tokens,
                    model: parsed.model().map(String::from),
                });
            }
        }

        let role = match parsed.role() {
            Some("assistant") => TranscriptRole::Assistant,
            Some("user") => TranscriptRole::User,
            Some("system") => TranscriptRole::System,
            _ => continue,
        };

        if let Some(serde_json::Value::String(s)) = parsed.content() {
            entries.push(TranscriptEntry {
                role,
                content: s.clone(),
                tool_call_id: None,
            });
        } else if let Some(serde_json::Value::Array(blocks)) = parsed.content() {
            for block in blocks {
                if let Ok(ContentBlock::Text { text }) =
                    serde_json::from_value::<ContentBlock>(block.clone())
                    && !text.is_empty()
                {
                    entries.push(TranscriptEntry {
                        role,
                        content: text,
                        tool_call_id: None,
                    });
                }
            }
        }
    }

    (entries, token_data, slug_found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_messages() {
        let jsonl = r#"{"role":"user","content":"Hello, help me refactor"}
{"role":"assistant","content":"I'll help you refactor the code.","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"}
{"role":"user","content":"Thanks!"}"#;

        let (entries, token_data, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].role, TranscriptRole::User);
        assert_eq!(entries[0].content, "Hello, help me refactor");
        assert_eq!(entries[1].role, TranscriptRole::Assistant);
        assert_eq!(entries[2].role, TranscriptRole::User);

        assert_eq!(token_data.len(), 1);
        assert_eq!(token_data[0].input_tokens, 100);
        assert_eq!(token_data[0].output_tokens, 50);
        assert_eq!(
            token_data[0].model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn parse_content_array_with_text() {
        let jsonl =
            r#"{"role":"assistant","content":[{"type":"text","text":"Let me read the file."}]}"#;

        let (entries, _, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "Let me read the file.");
    }

    #[test]
    fn parse_skips_invalid_lines() {
        let jsonl = "not valid json\n{\"role\":\"user\",\"content\":\"ok\"}\n{broken";

        let (entries, _, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "ok");
    }

    #[test]
    fn parse_skips_lines_without_role() {
        let jsonl = r#"{"type":"metadata","session_id":"abc"}
{"role":"user","content":"hello"}"#;

        let (entries, _, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn parse_multiple_usage_blocks() {
        let jsonl = r#"{"role":"assistant","content":"a","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":10,"cache_creation_input_tokens":5},"model":"claude-sonnet-4-20250514"}
{"role":"assistant","content":"b","usage":{"input_tokens":200,"output_tokens":80,"cache_read_input_tokens":20,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"}"#;

        let (_, token_data, _) = parse_transcript_str(jsonl);
        assert_eq!(token_data.len(), 2);
        assert_eq!(token_data[0].input_tokens, 100);
        assert_eq!(token_data[0].cache_read_input_tokens, 10);
        assert_eq!(token_data[1].input_tokens, 200);
    }

    #[tokio::test]
    async fn parse_file_with_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = r#"{"role":"user","content":"first"}
{"role":"assistant","content":"second"}
{"role":"user","content":"third"}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        // Parse from beginning
        let (entries, new_offset, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(new_offset, content.len() as u64);

        // Parse from offset (should return nothing new)
        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), new_offset)
            .await
            .unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_system_role() {
        let jsonl = r#"{"role":"system","content":"You are an assistant."}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, TranscriptRole::System);
        assert_eq!(entries[0].content, "You are an assistant.");
    }

    #[test]
    fn parse_unknown_role_is_skipped() {
        let jsonl = r#"{"role":"tool","content":"some output"}
{"role":"unknown_role","content":"skip me"}
{"role":"user","content":"keep me"}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "keep me");
    }

    #[test]
    fn parse_empty_input() {
        let (entries, token_data, _) = parse_transcript_str("");
        assert!(entries.is_empty());
        assert!(token_data.is_empty());
    }

    #[test]
    fn parse_only_whitespace_lines() {
        let jsonl = "   \n  \n\n   ";
        let (entries, _, _) = parse_transcript_str(jsonl);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_content_array_with_tool_use() {
        let jsonl = r#"{"role":"assistant","content":[{"type":"tool_use","id":"toolu_abc","name":"Read","input":{"file_path":"/src/main.rs"}}]}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        // parse_transcript_str only extracts Text blocks from arrays, not tool_use
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_content_array_with_empty_text() {
        let jsonl = r#"{"role":"assistant","content":[{"type":"text","text":""}]}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        // Empty text blocks should be skipped
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_content_array_mixed_blocks() {
        let jsonl = r#"{"role":"assistant","content":[{"type":"text","text":"Hello"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}},{"type":"text","text":"World"}]}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        // parse_transcript_str extracts only Text blocks
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].content, "Hello");
        assert_eq!(entries[1].content, "World");
    }

    #[test]
    fn parse_usage_with_no_model() {
        let jsonl = r#"{"role":"assistant","content":"text","usage":{"input_tokens":50,"output_tokens":25,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#;
        let (entries, token_data, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(token_data.len(), 1);
        assert_eq!(token_data[0].input_tokens, 50);
        assert!(token_data[0].model.is_none());
    }

    #[test]
    fn parse_content_as_non_string_non_array() {
        // Content is a number - should be skipped
        let jsonl = r#"{"role":"user","content":42}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_line_without_content() {
        let jsonl = r#"{"role":"user"}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_line_with_type_field() {
        let jsonl = r#"{"type":"summary","role":"assistant","content":"Summary text"}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "Summary text");
    }

    #[tokio::test]
    async fn parse_file_nonexistent_returns_error() {
        let result = parse_transcript_file("/nonexistent/path/file.jsonl", 0).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parse_file_offset_beyond_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.jsonl");
        tokio::fs::write(&path, r#"{"role":"user","content":"hi"}"#)
            .await
            .unwrap();

        let (entries, offset, token_data, _) = parse_transcript_file(path.to_str().unwrap(), 99999)
            .await
            .unwrap();
        assert!(entries.is_empty());
        assert!(token_data.is_empty());
        // offset should be total file length
        assert!(offset <= 99999);
    }

    #[tokio::test]
    async fn parse_file_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incremental.jsonl");

        // Write initial content
        let first = r#"{"role":"user","content":"first"}
"#;
        tokio::fs::write(&path, first).await.unwrap();

        let (entries, offset1, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "first");

        // Append more content
        let second = r#"{"role":"assistant","content":"second"}
"#;
        let mut full = first.to_string();
        full.push_str(second);
        tokio::fs::write(&path, &full).await.unwrap();

        // Parse from offset - should only get new entry
        let (entries, offset2, _, _) = parse_transcript_file(path.to_str().unwrap(), offset1)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "second");
        assert!(offset2 > offset1);
    }

    #[tokio::test]
    async fn parse_file_with_tool_use_content_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_use.jsonl");
        let content = r#"{"role":"assistant","content":[{"type":"text","text":"Let me read that."},{"type":"tool_use","id":"toolu_xyz","name":"Read","input":{"file_path":"/main.rs"}}]}
{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_xyz","content":"fn main() {}"}]}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        // Should have: text block, tool_use block, tool_result block
        assert!(entries.len() >= 2);
        // First entry should be the text
        assert_eq!(entries[0].role, TranscriptRole::Assistant);
        assert_eq!(entries[0].content, "Let me read that.");
        // Second should be tool use
        assert_eq!(entries[1].role, TranscriptRole::Tool);
        assert!(entries[1].content.contains("Read"));
        assert!(entries[1].tool_call_id.is_some());
    }

    #[tokio::test]
    async fn parse_file_tool_result_with_long_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long_result.jsonl");
        let long_content = "x".repeat(1000);
        let content = format!(
            r#"{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_long","content":"{long_content}"}}]}}
"#
        );
        tokio::fs::write(&path, &content).await.unwrap();

        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, TranscriptRole::Tool);
        // Content should be truncated with "..."
        assert!(entries[0].content.len() <= 510);
        assert!(entries[0].content.ends_with("..."));
    }

    #[test]
    fn token_usage_data_default() {
        let data = TokenUsageData::default();
        assert_eq!(data.input_tokens, 0);
        assert_eq!(data.output_tokens, 0);
        assert_eq!(data.cache_read_input_tokens, 0);
        assert_eq!(data.cache_creation_input_tokens, 0);
        assert!(data.model.is_none());
    }

    #[test]
    fn parse_content_array_with_other_block_type() {
        let jsonl = r#"{"role":"assistant","content":[{"type":"image","source":"base64data"},{"type":"text","text":"visible"}]}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "visible");
    }

    #[tokio::test]
    async fn parse_file_with_tool_use_truncates_long_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long_input.jsonl");
        let long_input = "y".repeat(500);
        let content = format!(
            r#"{{"role":"assistant","content":[{{"type":"tool_use","id":"toolu_trunc","name":"Write","input":{{"content":"{long_input}"}}}}]}}
"#
        );
        tokio::fs::write(&path, &content).await.unwrap();

        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert!(entries[0].content.contains("Write"));
        // Input should be truncated if > 200 chars when serialized
        if entries[0].content.len() > 220 {
            assert!(entries[0].content.contains("..."));
        }
    }

    #[tokio::test]
    async fn parse_file_tool_use_no_id_or_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_no_id.jsonl");
        let content = r#"{"role":"assistant","content":[{"type":"tool_use","input":{"key":"value"}}]}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, TranscriptRole::Tool);
        assert!(entries[0].content.contains("unknown"));
        assert!(entries[0].tool_call_id.is_none());
    }

    #[tokio::test]
    async fn parse_file_tool_result_no_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_result_empty.jsonl");
        let content = r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_empty"}]}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, TranscriptRole::Tool);
        assert!(entries[0].content.is_empty());
        assert!(entries[0].tool_call_id.is_some());
    }

    #[tokio::test]
    async fn parse_file_tool_result_no_tool_use_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_result_no_id.jsonl");
        let content = r#"{"role":"user","content":[{"type":"tool_result","content":"output data"}]}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, TranscriptRole::Tool);
        assert!(entries[0].tool_call_id.is_none());
    }

    #[tokio::test]
    async fn parse_file_other_content_block_type() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other_block.jsonl");
        let content = r#"{"role":"assistant","content":[{"type":"image","source":"data"},{"type":"text","text":"visible text"}]}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        // "image" type is Other and ignored, only text block kept
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "visible text");
    }

    #[tokio::test]
    async fn parse_file_empty_text_block_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty_text.jsonl");
        let content = r#"{"role":"assistant","content":[{"type":"text","text":""},{"type":"text","text":"real text"}]}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "real text");
    }

    #[tokio::test]
    async fn parse_file_content_as_non_string_non_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("non_string.jsonl");
        let content = r#"{"role":"user","content":42}
{"role":"user","content":true}
{"role":"user","content":{"nested":"object"}}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        // None of these should produce entries
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn parse_file_with_usage_extracts_token_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.jsonl");
        let content = r#"{"role":"assistant","content":"response 1","usage":{"input_tokens":500,"output_tokens":200,"cache_read_input_tokens":100,"cache_creation_input_tokens":50},"model":"claude-sonnet-4-20250514"}
{"role":"assistant","content":"response 2","usage":{"input_tokens":1000,"output_tokens":400,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, token_data, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(token_data.len(), 2);
        assert_eq!(token_data[0].input_tokens, 500);
        assert_eq!(token_data[0].output_tokens, 200);
        assert_eq!(token_data[0].cache_read_input_tokens, 100);
        assert_eq!(token_data[0].cache_creation_input_tokens, 50);
        assert_eq!(token_data[1].input_tokens, 1000);
    }

    #[tokio::test]
    async fn parse_file_mixed_valid_invalid_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed.jsonl");
        let content = r#"{"role":"user","content":"first"}
not json at all
{"invalid": "no role field"}

{"role":"assistant","content":"second"}
{broken json
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].content, "first");
        assert_eq!(entries[1].content, "second");
    }

    #[tokio::test]
    async fn parse_file_tool_use_with_no_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_no_input.jsonl");
        let content = r#"{"role":"assistant","content":[{"type":"tool_use","id":"toolu_noinput","name":"ListFiles"}]}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, _, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, TranscriptRole::Tool);
        assert!(entries[0].content.contains("ListFiles"));
        assert!(entries[0].tool_call_id.is_some());
    }

    #[test]
    fn parse_str_with_system_role() {
        let jsonl = r#"{"role":"system","content":"System prompt here"}
{"role":"user","content":"User message"}
{"role":"assistant","content":"Response"}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].role, TranscriptRole::System);
        assert_eq!(entries[1].role, TranscriptRole::User);
        assert_eq!(entries[2].role, TranscriptRole::Assistant);
    }

    #[test]
    fn parse_str_content_array_skips_non_text_blocks() {
        // parse_transcript_str only extracts Text blocks from arrays
        let jsonl = r#"{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{}},{"type":"text","text":"explanation"},{"type":"tool_result","tool_use_id":"t1","content":"result"}]}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "explanation");
    }

    #[test]
    fn parse_str_content_null_is_skipped() {
        let jsonl = r#"{"role":"user","content":null}"#;
        let (entries, _, _) = parse_transcript_str(jsonl);
        assert!(entries.is_empty());
    }

    #[test]
    fn transcript_entry_clone() {
        let entry = TranscriptEntry {
            role: TranscriptRole::User,
            content: "test content".to_string(),
            tool_call_id: Some(Uuid::new_v4()),
        };
        let cloned = entry.clone();
        assert_eq!(cloned.role, entry.role);
        assert_eq!(cloned.content, entry.content);
        assert_eq!(cloned.tool_call_id, entry.tool_call_id);
    }

    #[test]
    fn parse_nested_claude_code_format() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello, help me refactor"}]},"requestId":"req-1","timestamp":"2025-01-01T00:00:00Z"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I'll help you refactor."}],"usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":10,"cache_creation_input_tokens":5},"model":"claude-sonnet-4-20250514"},"requestId":"req-2","timestamp":"2025-01-01T00:00:01Z"}"#;

        let (entries, token_data, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, TranscriptRole::User);
        assert_eq!(entries[0].content, "Hello, help me refactor");
        assert_eq!(entries[1].role, TranscriptRole::Assistant);
        assert_eq!(entries[1].content, "I'll help you refactor.");

        assert_eq!(token_data.len(), 1);
        assert_eq!(token_data[0].input_tokens, 100);
        assert_eq!(token_data[0].output_tokens, 50);
        assert_eq!(token_data[0].cache_read_input_tokens, 10);
        assert_eq!(token_data[0].cache_creation_input_tokens, 5);
        assert_eq!(
            token_data[0].model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn parse_nested_format_with_tool_use() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Let me read it."},{"type":"tool_use","id":"toolu_abc","name":"Read","input":{"file_path":"/src/main.rs"}}],"usage":{"input_tokens":200,"output_tokens":80,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"},"requestId":"req-3"}"#;

        let (entries, token_data, _) = parse_transcript_str(jsonl);
        // parse_transcript_str only extracts Text blocks
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "Let me read it.");

        assert_eq!(token_data.len(), 1);
        assert_eq!(token_data[0].input_tokens, 200);
    }

    #[test]
    fn parse_deduplicates_by_request_id() {
        // Multiple lines with the same requestId should only produce one TokenUsageData
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"first part"}],"usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"},"requestId":"req-dup"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"second part"}],"usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"},"requestId":"req-dup"}"#;

        let (entries, token_data, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 2); // both text entries appear
        assert_eq!(token_data.len(), 1); // usage deduplicated to one
        assert_eq!(token_data[0].input_tokens, 100);
    }

    #[test]
    fn parse_different_request_ids_not_deduplicated() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"a"}],"usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"},"requestId":"req-1"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"b"}],"usage":{"input_tokens":200,"output_tokens":80,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"},"requestId":"req-2"}"#;

        let (_, token_data, _) = parse_transcript_str(jsonl);
        assert_eq!(token_data.len(), 2);
        assert_eq!(token_data[0].input_tokens, 100);
        assert_eq!(token_data[1].input_tokens, 200);
    }

    #[test]
    fn parse_no_request_id_not_deduplicated() {
        // Lines without requestId are never considered duplicates (backward compat)
        let jsonl = r#"{"role":"assistant","content":"a","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"}
{"role":"assistant","content":"b","usage":{"input_tokens":200,"output_tokens":80,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"}"#;

        let (_, token_data, _) = parse_transcript_str(jsonl);
        assert_eq!(token_data.len(), 2);
    }

    #[test]
    fn parse_nested_format_without_usage() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"}]},"requestId":"req-no-usage"}"#;

        let (entries, token_data, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "hello");
        assert!(token_data.is_empty());
    }

    #[tokio::test]
    async fn parse_file_nested_claude_code_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested.jsonl");
        let content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"refactor this"}]},"requestId":"req-1"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Sure, I'll refactor."},{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/main.rs"}}],"usage":{"input_tokens":500,"output_tokens":200,"cache_read_input_tokens":100,"cache_creation_input_tokens":50},"model":"claude-sonnet-4-20250514"},"requestId":"req-2"}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, token_data, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        // user text + assistant text + tool_use
        assert!(entries.len() >= 2);
        assert_eq!(entries[0].role, TranscriptRole::User);
        assert_eq!(entries[0].content, "refactor this");
        assert_eq!(entries[1].role, TranscriptRole::Assistant);
        assert_eq!(entries[1].content, "Sure, I'll refactor.");

        assert_eq!(token_data.len(), 1);
        assert_eq!(token_data[0].input_tokens, 500);
        assert_eq!(token_data[0].cache_creation_input_tokens, 50);
    }

    #[tokio::test]
    async fn parse_file_deduplicates_by_request_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedup.jsonl");
        let content = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"part 1"}],"usage":{"input_tokens":300,"output_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"},"requestId":"dup-req"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"part 2"}],"usage":{"input_tokens":300,"output_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"},"requestId":"dup-req"}
"#;
        tokio::fs::write(&path, content).await.unwrap();

        let (entries, _, token_data, _) = parse_transcript_file(path.to_str().unwrap(), 0)
            .await
            .unwrap();

        assert_eq!(entries.len(), 2); // both entries present
        assert_eq!(token_data.len(), 1); // usage deduplicated
    }

    #[test]
    fn parse_extracts_slug() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"}]},"slug":"rename-myremote-to-zremote","requestId":"req-1"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]},"slug":"rename-myremote-to-zremote","requestId":"req-2"}"#;
        let (entries, _, slug) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 2);
        assert_eq!(slug.as_deref(), Some("rename-myremote-to-zremote"));
    }

    #[test]
    fn parse_no_slug_returns_none() {
        let jsonl = r#"{"role":"user","content":"hello"}"#;
        let (_, _, slug) = parse_transcript_str(jsonl);
        assert!(slug.is_none());
    }
}
