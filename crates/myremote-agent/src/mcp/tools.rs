use std::fmt::Write as _;

use super::KnowledgeMcpServer;

/// Return the list of MCP tools provided by this server.
pub fn tool_list() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "name": "knowledge_search",
            "description": "Semantic code search across project files using OpenViking indexing",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query (natural language or code pattern)"
                    },
                    "tier": {
                        "type": "string",
                        "enum": ["l0", "l1", "l2"],
                        "description": "Search tier: l0 (fast/shallow), l1 (balanced, default), l2 (deep/slow)"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results (default: 10, max: 20)"
                    }
                },
                "required": ["query"]
            }
        }),
        serde_json::json!({
            "name": "knowledge_memories",
            "description": "Query extracted project memories and learnings from past sessions",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "category": {
                        "type": "string",
                        "enum": ["pattern", "decision", "pitfall", "preference", "architecture", "convention"],
                        "description": "Filter by memory category"
                    },
                    "query": {
                        "type": "string",
                        "description": "Text search query to filter memories"
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "knowledge_context",
            "description": "Get high-level project understanding from accumulated knowledge",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
    ]
}

/// Handle a tool call and return the result.
pub async fn handle_tool_call(
    server: &KnowledgeMcpServer,
    params: &serde_json::Value,
) -> serde_json::Value {
    let tool_name = params
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    match tool_name {
        "knowledge_search" => handle_search(server, &arguments).await,
        "knowledge_memories" => handle_memories(server, &arguments).await,
        "knowledge_context" => handle_context(server).await,
        _ => tool_error(&format!("Unknown tool: {tool_name}")),
    }
}

async fn handle_search(
    server: &KnowledgeMcpServer,
    args: &serde_json::Value,
) -> serde_json::Value {
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .unwrap_or("");
    if query.is_empty() {
        return tool_error("query parameter is required");
    }

    let tier = args
        .get("tier")
        .and_then(|t| t.as_str())
        .unwrap_or("l1");
    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |m| m.min(20) as u32);

    match server
        .client
        .search(&server.namespace, query, tier, max_results)
        .await
    {
        Ok(results) => {
            if results.is_empty() {
                return tool_text("No results found.");
            }
            let mut output = String::new();
            for (i, r) in results.iter().enumerate().take(10) {
                let lines = match (r.line_start, r.line_end) {
                    (Some(s), Some(e)) => format!("L{s}-L{e}"),
                    (Some(s), None) => format!("L{s}"),
                    _ => String::new(),
                };
                let snippet = if r.snippet.len() > 200 {
                    format!("{}...", &r.snippet[..200])
                } else {
                    r.snippet.clone()
                };
                let _ = write!(
                    output,
                    "{}. {} {} (score: {:.2})\n{}\n\n",
                    i + 1,
                    r.path,
                    lines,
                    r.score,
                    snippet
                );
            }
            tool_text(&output)
        }
        Err(e) => tool_error(&format!("Search failed: {e}")),
    }
}

async fn handle_memories(
    server: &KnowledgeMcpServer,
    args: &serde_json::Value,
) -> serde_json::Value {
    let category_filter = args.get("category").and_then(|c| c.as_str());
    let query_filter = args
        .get("query")
        .and_then(|q| q.as_str())
        .map(str::to_lowercase);

    let cache = crate::knowledge::read_memory_cache_for_project(
        server.project_path.to_str().unwrap_or(""),
    )
    .await;

    let filtered: Vec<_> = cache
        .iter()
        .filter(|m| {
            if let Some(cat) = category_filter {
                let cat_str = serde_json::to_value(m.category)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default();
                if cat_str != cat {
                    return false;
                }
            }
            if let Some(ref q) = query_filter
                && !m.key.to_lowercase().contains(q)
                && !m.content.to_lowercase().contains(q)
            {
                return false;
            }
            true
        })
        .take(20)
        .collect();

    if filtered.is_empty() {
        return tool_text("No memories found matching the criteria.");
    }

    let mut output = String::new();
    for m in &filtered {
        let cat_str = serde_json::to_value(m.category)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        let _ = write!(
            output,
            "**{}** [{}] (confidence: {:.0}%)\n{}\n\n",
            m.key,
            cat_str,
            m.confidence * 100.0,
            m.content
        );
    }

    tool_text(&output)
}

async fn handle_context(server: &KnowledgeMcpServer) -> serde_json::Value {
    let claude_md_path = server.project_path.join(".claude/CLAUDE.md");
    let marker = "<!-- MyRemote Knowledge (auto-generated, do not edit below) -->";

    if let Ok(data) = tokio::fs::read_to_string(&claude_md_path).await {
        let content = if let Some(pos) = data.find(marker) {
            data[pos + marker.len()..].trim().to_string()
        } else {
            data
        };
        tool_text(&content)
    } else {
        // Try synthesizing from memories
        let cache = crate::knowledge::read_memory_cache_for_project(
            server.project_path.to_str().unwrap_or(""),
        )
        .await;
        if cache.is_empty() {
            tool_text("No project knowledge available. Run knowledge indexing first.")
        } else {
            let mut output = String::from("# Project Knowledge (from cached memories)\n\n");
            for m in cache.iter().take(20) {
                output.push_str(&format!("- **{}**: {}\n", m.key, m.content));
            }
            tool_text(&output)
        }
    }
}

fn tool_text(text: &str) -> serde_json::Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": text
        }]
    })
}

fn tool_error(message: &str) -> serde_json::Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": message
        }],
        "isError": true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_has_three_tools() {
        let tools = tool_list();
        assert_eq!(tools.len(), 3);
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"knowledge_search"));
        assert!(names.contains(&"knowledge_memories"));
        assert!(names.contains(&"knowledge_context"));
    }

    #[test]
    fn tool_list_has_input_schemas() {
        for tool in tool_list() {
            assert!(tool["inputSchema"].is_object());
        }
    }

    #[test]
    fn tool_text_format() {
        let result = tool_text("hello");
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "hello");
    }

    #[test]
    fn tool_error_format() {
        let result = tool_error("bad");
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "bad");
    }

    #[tokio::test]
    async fn handle_tool_call_unknown() {
        let server = KnowledgeMcpServer::new(std::path::PathBuf::from("/tmp/test"), 8741);
        let params = serde_json::json!({"name": "unknown_tool"});
        let result = handle_tool_call(&server, &params).await;
        assert_eq!(result["isError"], true);
    }

    #[tokio::test]
    async fn handle_search_missing_query() {
        let server = KnowledgeMcpServer::new(std::path::PathBuf::from("/tmp/test"), 8741);
        let params = serde_json::json!({"name": "knowledge_search", "arguments": {}});
        let result = handle_tool_call(&server, &params).await;
        assert_eq!(result["isError"], true);
    }

    #[tokio::test]
    async fn handle_context_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let server = KnowledgeMcpServer::new(dir.path().to_path_buf(), 8741);
        let params = serde_json::json!({"name": "knowledge_context", "arguments": {}});
        let result = handle_tool_call(&server, &params).await;
        // Should not error, just return a helpful message
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("No project knowledge"));
    }

    #[tokio::test]
    async fn handle_memories_empty_cache() {
        let dir = tempfile::tempdir().unwrap();
        let server = KnowledgeMcpServer::new(dir.path().to_path_buf(), 8741);
        let params = serde_json::json!({"name": "knowledge_memories", "arguments": {}});
        let result = handle_tool_call(&server, &params).await;
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("No memories found"));
    }
}
