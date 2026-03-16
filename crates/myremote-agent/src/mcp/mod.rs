mod tools;

use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::knowledge::client::OvClient;
use crate::knowledge::project_name_from_path;

/// MCP server state shared across tool calls.
pub struct KnowledgeMcpServer {
    client: OvClient,
    project_path: PathBuf,
    namespace: String,
}

impl KnowledgeMcpServer {
    fn new(project_path: PathBuf, ov_port: u16) -> Self {
        let name = project_name_from_path(project_path.to_str().unwrap_or("project"));
        let namespace = format!("viking://resources/{name}/");
        Self {
            client: OvClient::new(ov_port, None),
            project_path,
            namespace,
        }
    }
}

/// Run the MCP server on stdio using JSON-RPC.
pub async fn run_mcp_server(project_path: PathBuf, ov_port: u16) {
    tracing::info!(
        project = %project_path.display(),
        ov_port,
        "starting MCP server (stdio)"
    );

    let server = KnowledgeMcpServer::new(project_path, ov_port);
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let response = handle_jsonrpc_message(&server, line).await;
                if let Some(resp) = response {
                    let resp_str = serde_json::to_string(&resp).unwrap_or_default();
                    if stdout
                        .write_all(format!("{resp_str}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                    if stdout.flush().await.is_err() {
                        break;
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to read from stdin");
                break;
            }
        }
    }
}

/// Handle a single JSON-RPC message and return a response (or None for notifications).
async fn handle_jsonrpc_message(
    server: &KnowledgeMcpServer,
    message: &str,
) -> Option<serde_json::Value> {
    let parsed: serde_json::Value = match serde_json::from_str(message) {
        Ok(v) => v,
        Err(e) => {
            return Some(jsonrpc_error(
                serde_json::Value::Null,
                -32700,
                &format!("Parse error: {e}"),
            ));
        }
    };

    let id = parsed.get("id").cloned();
    let method = parsed.get("method").and_then(|m| m.as_str()).unwrap_or("");

    // Notifications (no id) get no response
    let id = match id {
        Some(id) if !id.is_null() => id,
        _ if method == "notifications/initialized" || method == "notifications/cancelled" => {
            return None;
        }
        _ => serde_json::Value::Null,
    };

    match method {
        "initialize" => Some(jsonrpc_ok(
            id,
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {},
                    "resources": {}
                },
                "serverInfo": {
                    "name": "myremote-knowledge",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),
        "tools/list" => Some(jsonrpc_ok(id, serde_json::json!({ "tools": tools::tool_list() }))),
        "tools/call" => {
            let params = parsed.get("params").cloned().unwrap_or(serde_json::Value::Null);
            let result = tools::handle_tool_call(server, &params).await;
            Some(jsonrpc_ok(id, result))
        }
        "resources/list" => {
            let resources = resource_list(server);
            Some(jsonrpc_ok(id, serde_json::json!({ "resources": resources })))
        }
        "resources/read" => {
            let uri = parsed
                .get("params")
                .and_then(|p| p.get("uri"))
                .and_then(|u| u.as_str())
                .unwrap_or("");
            let result = read_resource(server, uri).await;
            Some(jsonrpc_ok(id, result))
        }
        "ping" => Some(jsonrpc_ok(id, serde_json::json!({}))),
        _ => Some(jsonrpc_error(id, -32601, &format!("Method not found: {method}"))),
    }
}

fn resource_list(_server: &KnowledgeMcpServer) -> Vec<serde_json::Value> {
    let categories = ["pattern", "decision", "pitfall", "architecture", "convention"];
    let mut resources = vec![serde_json::json!({
        "uri": "myremote://context",
        "name": "Project Context",
        "description": "Auto-generated CLAUDE.md section with project knowledge",
        "mimeType": "text/markdown"
    })];
    for cat in &categories {
        resources.push(serde_json::json!({
            "uri": format!("myremote://memories/{cat}"),
            "name": format!("{cat} memories"),
            "description": format!("Extracted {cat} memories for the project"),
            "mimeType": "text/plain"
        }));
    }
    resources
}

async fn read_resource(server: &KnowledgeMcpServer, uri: &str) -> serde_json::Value {
    if uri == "myremote://context" {
        // Read the auto-generated section from CLAUDE.md
        let claude_md_path = server.project_path.join(".claude/CLAUDE.md");
        let content = if let Ok(data) = tokio::fs::read_to_string(&claude_md_path).await {
            let marker = "<!-- MyRemote Knowledge (auto-generated, do not edit below) -->";
            if let Some(pos) = data.find(marker) {
                data[pos + marker.len()..].trim().to_string()
            } else {
                data
            }
        } else {
            "No CLAUDE.md found for this project.".to_string()
        };
        serde_json::json!({
            "contents": [{
                "uri": uri,
                "mimeType": "text/markdown",
                "text": content
            }]
        })
    } else if let Some(category) = uri.strip_prefix("myremote://memories/") {
        let cache = crate::knowledge::read_memory_cache_for_project(
            server.project_path.to_str().unwrap_or(""),
        )
        .await;
        let filtered: Vec<_> = cache
            .iter()
            .filter(|m| {
                serde_json::to_value(m.category)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .is_some_and(|c| c == category)
            })
            .collect();

        let text = if filtered.is_empty() {
            format!("No {category} memories found.")
        } else {
            filtered
                .iter()
                .map(|m| format!("## {}\n{}\n(confidence: {:.0}%)", m.key, m.content, m.confidence * 100.0))
                .collect::<Vec<_>>()
                .join("\n\n")
        };

        serde_json::json!({
            "contents": [{
                "uri": uri,
                "mimeType": "text/plain",
                "text": text
            }]
        })
    } else {
        serde_json::json!({
            "contents": [],
            "isError": true
        })
    }
}

fn jsonrpc_ok(id: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn jsonrpc_error(id: serde_json::Value, code: i32, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonrpc_ok_format() {
        let result = jsonrpc_ok(serde_json::json!(1), serde_json::json!({"status": "ok"}));
        assert_eq!(result["jsonrpc"], "2.0");
        assert_eq!(result["id"], 1);
        assert_eq!(result["result"]["status"], "ok");
    }

    #[test]
    fn jsonrpc_error_format() {
        let result = jsonrpc_error(serde_json::json!(2), -32601, "Method not found");
        assert_eq!(result["jsonrpc"], "2.0");
        assert_eq!(result["id"], 2);
        assert_eq!(result["error"]["code"], -32601);
        assert_eq!(result["error"]["message"], "Method not found");
    }

    #[test]
    fn resource_list_has_context_and_categories() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let resources = resource_list(&server);
        assert!(resources.len() >= 6); // context + 5 categories
        assert_eq!(resources[0]["uri"], "myremote://context");
    }

    #[tokio::test]
    async fn handle_initialize() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn handle_tools_list() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
    }

    #[tokio::test]
    async fn handle_ping() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert!(resp["result"].is_object());
    }

    #[tokio::test]
    async fn handle_unknown_method() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":4,"method":"unknown/method"}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn handle_notification_returns_none() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let resp = handle_jsonrpc_message(&server, msg).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn handle_cancelled_notification_returns_none() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#;
        let resp = handle_jsonrpc_message(&server, msg).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn handle_parse_error() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = "not valid json at all {{{";
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert_eq!(resp["error"]["code"], -32700);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Parse error"));
    }

    #[tokio::test]
    async fn handle_initialize_has_server_info() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":10,"method":"initialize","params":{}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert_eq!(resp["id"], 10);
        assert!(resp["result"]["serverInfo"]["name"]
            .as_str()
            .unwrap()
            .contains("myremote"));
        assert!(resp["result"]["serverInfo"]["version"].is_string());
        assert!(resp["result"]["capabilities"]["resources"].is_object());
    }

    #[tokio::test]
    async fn handle_resources_list() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":5,"method":"resources/list","params":{}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        let resources = resp["result"]["resources"].as_array().unwrap();
        assert!(resources.len() >= 6);
        // First resource should be the context resource
        assert_eq!(resources[0]["uri"], "myremote://context");
        // Check that category resources exist
        let uris: Vec<&str> = resources.iter().map(|r| r["uri"].as_str().unwrap()).collect();
        assert!(uris.contains(&"myremote://memories/pattern"));
        assert!(uris.contains(&"myremote://memories/decision"));
        assert!(uris.contains(&"myremote://memories/pitfall"));
        assert!(uris.contains(&"myremote://memories/architecture"));
        assert!(uris.contains(&"myremote://memories/convention"));
    }

    #[tokio::test]
    async fn handle_resources_read_context_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let server = KnowledgeMcpServer::new(dir.path().to_path_buf(), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{"uri":"myremote://context"}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        let contents = resp["result"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["uri"], "myremote://context");
        assert_eq!(contents[0]["mimeType"], "text/markdown");
    }

    #[tokio::test]
    async fn handle_resources_read_context_with_marker() {
        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let claude_md = claude_dir.join("CLAUDE.md");
        std::fs::write(
            &claude_md,
            "# Project\nSome content\n<!-- MyRemote Knowledge (auto-generated, do not edit below) -->\n## Knowledge\nExtracted knowledge here",
        )
        .unwrap();

        let server = KnowledgeMcpServer::new(dir.path().to_path_buf(), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":7,"method":"resources/read","params":{"uri":"myremote://context"}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
        assert!(text.contains("Knowledge"));
        assert!(text.contains("Extracted knowledge here"));
        // Should NOT contain content before the marker
        assert!(!text.contains("Some content"));
    }

    #[tokio::test]
    async fn handle_resources_read_unknown_uri() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":8,"method":"resources/read","params":{"uri":"myremote://unknown"}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
        assert_eq!(resp["result"]["contents"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn handle_resources_read_no_uri_param() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":9,"method":"resources/read","params":{}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        // Empty string URI won't match any pattern, so returns error
        assert_eq!(resp["result"]["isError"], true);
    }

    #[tokio::test]
    async fn handle_tools_call_with_missing_params() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":10,"method":"tools/call"}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        // Missing params should return unknown tool error
        assert_eq!(resp["result"]["isError"], true);
    }

    #[tokio::test]
    async fn handle_method_with_null_id() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert!(resp["result"].is_object());
        assert!(resp["id"].is_null());
    }

    #[tokio::test]
    async fn handle_string_id() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":"req-1","method":"ping"}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert_eq!(resp["id"], "req-1");
    }

    #[test]
    fn resource_list_all_have_required_fields() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        for resource in resource_list(&server) {
            assert!(resource["uri"].is_string());
            assert!(resource["name"].is_string());
            assert!(resource["description"].is_string());
            assert!(resource["mimeType"].is_string());
        }
    }

    #[test]
    fn knowledge_mcp_server_namespace_format() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/home/user/my-project"), 8741);
        assert!(server.namespace.starts_with("viking://resources/"));
        assert!(server.namespace.ends_with('/'));
    }

    #[tokio::test]
    async fn handle_resources_read_memories_category() {
        let dir = tempfile::tempdir().unwrap();
        let server = KnowledgeMcpServer::new(dir.path().to_path_buf(), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":11,"method":"resources/read","params":{"uri":"myremote://memories/pattern"}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
        // With no cached memories, should say none found
        assert!(text.contains("No pattern memories found"));
    }

    #[tokio::test]
    async fn handle_tools_call_search_no_query() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"knowledge_search","arguments":{}}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("query parameter is required"));
    }

    #[tokio::test]
    async fn handle_tools_call_context() {
        let dir = tempfile::tempdir().unwrap();
        let server = KnowledgeMcpServer::new(dir.path().to_path_buf(), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"knowledge_context","arguments":{}}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        // Should return a result (no error), with some text
        assert!(resp["result"]["content"][0]["text"].is_string());
    }

    #[tokio::test]
    async fn handle_tools_call_memories() {
        let dir = tempfile::tempdir().unwrap();
        let server = KnowledgeMcpServer::new(dir.path().to_path_buf(), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"knowledge_memories","arguments":{"category":"pattern"}}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("No memories found"));
    }

    #[tokio::test]
    async fn handle_resources_read_multiple_categories() {
        let dir = tempfile::tempdir().unwrap();
        let server = KnowledgeMcpServer::new(dir.path().to_path_buf(), 8741);

        for category in &["decision", "pitfall", "architecture", "convention"] {
            let msg = format!(
                r#"{{"jsonrpc":"2.0","id":20,"method":"resources/read","params":{{"uri":"myremote://memories/{category}"}}}}"#
            );
            let resp = handle_jsonrpc_message(&server, &msg).await.unwrap();
            let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
            assert!(text.contains(&format!("No {category} memories found")));
        }
    }

    #[tokio::test]
    async fn handle_context_with_claude_md_no_marker() {
        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let claude_md = claude_dir.join("CLAUDE.md");
        std::fs::write(&claude_md, "# My Project\nContent without marker").unwrap();

        let server = KnowledgeMcpServer::new(dir.path().to_path_buf(), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":21,"method":"resources/read","params":{"uri":"myremote://context"}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
        // Without marker, returns entire file
        assert!(text.contains("My Project"));
        assert!(text.contains("Content without marker"));
    }

    #[tokio::test]
    async fn handle_jsonrpc_preserves_integer_id() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert_eq!(resp["id"], 42);
        assert_eq!(resp["jsonrpc"], "2.0");
    }

    #[tokio::test]
    async fn handle_method_without_id_and_not_notification() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        // Method with no id that is NOT a notification - should get null id
        let msg = r#"{"jsonrpc":"2.0","method":"ping"}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert!(resp["id"].is_null());
        assert!(resp["result"].is_object());
    }

    #[tokio::test]
    async fn handle_tools_call_unknown_tool() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":30,"method":"tools/call","params":{"name":"nonexistent_tool","arguments":{}}}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Unknown tool"));
    }

    #[tokio::test]
    async fn handle_tools_call_null_params() {
        let server = KnowledgeMcpServer::new(PathBuf::from("/tmp/test"), 8741);
        let msg = r#"{"jsonrpc":"2.0","id":31,"method":"tools/call","params":null}"#;
        let resp = handle_jsonrpc_message(&server, msg).await.unwrap();
        // null params -> name is "", treated as unknown tool
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn jsonrpc_ok_with_complex_result() {
        let result = jsonrpc_ok(
            serde_json::json!("id-str"),
            serde_json::json!({"tools": [{"name": "test"}]}),
        );
        assert_eq!(result["id"], "id-str");
        assert!(result["result"]["tools"].is_array());
    }

    #[test]
    fn jsonrpc_error_with_null_id() {
        let result = jsonrpc_error(serde_json::Value::Null, -32700, "Parse error");
        assert!(result["id"].is_null());
        assert_eq!(result["error"]["code"], -32700);
    }
}
