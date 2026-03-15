use myremote_protocol::TranscriptRole;
use serde::Deserialize;
use uuid::Uuid;

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

/// A message entry in the transcript JSONL.
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
pub async fn parse_transcript_file(
    path: &str,
    offset: u64,
) -> Result<(Vec<TranscriptEntry>, u64, Vec<TokenUsageData>), std::io::Error> {
    let data = tokio::fs::read(path).await?;
    let total_len = data.len() as u64;

    if offset >= total_len {
        return Ok((Vec::new(), total_len, Vec::new()));
    }

    let slice = &data[offset as usize..];
    let text = String::from_utf8_lossy(slice);

    let mut entries = Vec::new();
    let mut token_data = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(parsed) = serde_json::from_str::<TranscriptLine>(line) else {
            continue;
        };

        // Extract token usage
        if let Some(usage) = &parsed.usage {
            token_data.push(TokenUsageData {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_input_tokens: usage.cache_read_input_tokens,
                cache_creation_input_tokens: usage.cache_creation_input_tokens,
                model: parsed.model.clone(),
            });
        }

        // Extract message content
        let role = match parsed.role.as_deref() {
            Some("assistant") => TranscriptRole::Assistant,
            Some("user") => TranscriptRole::User,
            Some("system") => TranscriptRole::System,
            _ => continue,
        };

        if let Some(content) = &parsed.content {
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
                        if let Ok(cb) =
                            serde_json::from_value::<ContentBlock>(block.clone())
                        {
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
                                    let tool_call_id = id.as_deref().map(|s| {
                                        Uuid::new_v5(
                                            &Uuid::NAMESPACE_URL,
                                            s.as_bytes(),
                                        )
                                    });
                                    let desc = format!(
                                        "Tool: {} | Input: {}",
                                        name.as_deref().unwrap_or("unknown"),
                                        input
                                            .as_ref()
                                            .map(|v| {
                                                let s = serde_json::to_string(v)
                                                    .unwrap_or_default();
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
                                    let tool_call_id =
                                        tool_use_id.as_deref().map(|s| {
                                            Uuid::new_v5(
                                                &Uuid::NAMESPACE_URL,
                                                s.as_bytes(),
                                            )
                                        });
                                    let result_text = content
                                        .as_ref()
                                        .map(|v| {
                                            let s = serde_json::to_string(v)
                                                .unwrap_or_default();
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

    Ok((entries, total_len, token_data))
}

/// Parse transcript content from a string (for testing).
pub fn parse_transcript_str(text: &str) -> (Vec<TranscriptEntry>, Vec<TokenUsageData>) {
    let mut entries = Vec::new();
    let mut token_data = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(parsed) = serde_json::from_str::<TranscriptLine>(line) else {
            continue;
        };

        if let Some(usage) = &parsed.usage {
            token_data.push(TokenUsageData {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_input_tokens: usage.cache_read_input_tokens,
                cache_creation_input_tokens: usage.cache_creation_input_tokens,
                model: parsed.model.clone(),
            });
        }

        let role = match parsed.role.as_deref() {
            Some("assistant") => TranscriptRole::Assistant,
            Some("user") => TranscriptRole::User,
            Some("system") => TranscriptRole::System,
            _ => continue,
        };

        if let Some(serde_json::Value::String(s)) = &parsed.content {
            entries.push(TranscriptEntry {
                role,
                content: s.clone(),
                tool_call_id: None,
            });
        } else if let Some(serde_json::Value::Array(blocks)) = &parsed.content {
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

    (entries, token_data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_messages() {
        let jsonl = r#"{"role":"user","content":"Hello, help me refactor"}
{"role":"assistant","content":"I'll help you refactor the code.","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"}
{"role":"user","content":"Thanks!"}"#;

        let (entries, token_data) = parse_transcript_str(jsonl);
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
        let jsonl = r#"{"role":"assistant","content":[{"type":"text","text":"Let me read the file."}]}"#;

        let (entries, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "Let me read the file.");
    }

    #[test]
    fn parse_skips_invalid_lines() {
        let jsonl = "not valid json\n{\"role\":\"user\",\"content\":\"ok\"}\n{broken";

        let (entries, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "ok");
    }

    #[test]
    fn parse_skips_lines_without_role() {
        let jsonl = r#"{"type":"metadata","session_id":"abc"}
{"role":"user","content":"hello"}"#;

        let (entries, _) = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn parse_multiple_usage_blocks() {
        let jsonl = r#"{"role":"assistant","content":"a","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":10,"cache_creation_input_tokens":5},"model":"claude-sonnet-4-20250514"}
{"role":"assistant","content":"b","usage":{"input_tokens":200,"output_tokens":80,"cache_read_input_tokens":20,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"}"#;

        let (_, token_data) = parse_transcript_str(jsonl);
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
        let (entries, new_offset, _) =
            parse_transcript_file(path.to_str().unwrap(), 0)
                .await
                .unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(new_offset, content.len() as u64);

        // Parse from offset (should return nothing new)
        let (entries, _, _) =
            parse_transcript_file(path.to_str().unwrap(), new_offset)
                .await
                .unwrap();
        assert!(entries.is_empty());
    }
}
