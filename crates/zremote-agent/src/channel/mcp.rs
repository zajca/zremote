use super::jsonrpc;
use super::tools;
use super::types::ChannelState;

/// Handle a single JSON-RPC message and return a response (or None for notifications).
pub async fn handle_jsonrpc_message(
    state: &ChannelState,
    message: &str,
) -> Option<serde_json::Value> {
    let parsed: serde_json::Value = match serde_json::from_str(message) {
        Ok(v) => v,
        Err(e) => {
            return Some(jsonrpc::jsonrpc_error(
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
        _ if method.starts_with("notifications/") => {
            handle_notification(state, method, &parsed).await;
            return None;
        }
        _ => serde_json::Value::Null,
    };

    match method {
        "initialize" => Some(jsonrpc::jsonrpc_ok(
            id,
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {},
                    "experimental": {
                        "claude/channel": {},
                        "claude/channel/permission": {}
                    }
                },
                "serverInfo": {
                    "name": "zremote-channel",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),
        "tools/list" => Some(jsonrpc::jsonrpc_ok(
            id,
            serde_json::json!({ "tools": tools::tool_list() }),
        )),
        "tools/call" => {
            let params = parsed.get("params").cloned().unwrap_or_default();
            let result = handle_tool_call(state, &params).await;
            Some(jsonrpc::jsonrpc_ok(id, result))
        }
        "ping" => Some(jsonrpc::jsonrpc_ok(id, serde_json::json!({}))),
        _ => Some(jsonrpc::jsonrpc_error(
            id,
            -32601,
            &format!("Method not found: {method}"),
        )),
    }
}

/// Handle MCP notifications (no response expected).
async fn handle_notification(state: &ChannelState, method: &str, parsed: &serde_json::Value) {
    match method {
        "notifications/initialized" | "notifications/cancelled" => {
            tracing::debug!(method, "received notification");
        }
        "notifications/claude/channel" => {
            let params = parsed
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            forward_channel_notification(state, &params).await;
        }
        "notifications/claude/channel/permission" => {
            let params = parsed
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            forward_permission_notification(state, &params).await;
        }
        _ => {
            tracing::debug!(method, "ignoring unknown notification");
        }
    }
}

/// Forward an incoming channel notification from CC to the agent callback.
async fn forward_channel_notification(state: &ChannelState, params: &serde_json::Value) {
    let url = format!(
        "{}/channel/{}/notification",
        state.agent_callback, state.session_id
    );
    if let Err(e) = state.http_client.post(&url).json(params).send().await {
        tracing::error!(error = %e, "failed to forward channel notification to agent");
    }
}

/// Forward a permission notification from CC to the agent callback.
async fn forward_permission_notification(state: &ChannelState, params: &serde_json::Value) {
    let url = format!(
        "{}/channel/{}/permission",
        state.agent_callback, state.session_id
    );
    if let Err(e) = state.http_client.post(&url).json(params).send().await {
        tracing::error!(error = %e, "failed to forward permission notification to agent");
    }
}

/// Handle a `tools/call` request.
async fn handle_tool_call(state: &ChannelState, params: &serde_json::Value) -> serde_json::Value {
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    match tools::handle_tool_call(name, &arguments) {
        Ok((tool_type, response)) => {
            // POST the response to the agent callback
            let url = format!("{}/channel/{tool_type}", state.agent_callback);
            match state.http_client.post(&url).json(&response).send().await {
                Ok(resp) if resp.status().is_success() => {
                    serde_json::json!({
                        "content": [{
                            "type": "text",
                            "text": "OK"
                        }]
                    })
                }
                Ok(resp) => {
                    let status = resp.status();
                    tracing::error!(%status, "agent callback returned error");
                    serde_json::json!({
                        "content": [{
                            "type": "text",
                            "text": format!("Agent callback error: {status}")
                        }],
                        "isError": true
                    })
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to POST to agent callback");
                    serde_json::json!({
                        "content": [{
                            "type": "text",
                            "text": format!("Failed to reach agent: {e}")
                        }],
                        "isError": true
                    })
                }
            }
        }
        Err(err) => {
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": err
                }],
                "isError": true
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_state() -> ChannelState {
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        ChannelState {
            session_id: Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:0".to_string(),
            stdio_tx: tx,
            http_client: reqwest::Client::new(),
        }
    }

    #[tokio::test]
    async fn handle_initialize() {
        let state = test_state();
        let msg = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let resp = handle_jsonrpc_message(&state, msg).await.unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert!(resp["result"]["capabilities"]["experimental"]["claude/channel"].is_object());
        assert!(
            resp["result"]["capabilities"]["experimental"]["claude/channel/permission"].is_object()
        );
        assert_eq!(resp["result"]["serverInfo"]["name"], "zremote-channel");
    }

    #[tokio::test]
    async fn handle_tools_list() {
        let state = test_state();
        let msg = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let resp = handle_jsonrpc_message(&state, msg).await.unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
    }

    #[tokio::test]
    async fn handle_ping() {
        let state = test_state();
        let msg = r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#;
        let resp = handle_jsonrpc_message(&state, msg).await.unwrap();
        assert!(resp["result"].is_object());
        assert_eq!(resp["id"], 3);
    }

    #[tokio::test]
    async fn handle_unknown_method() {
        let state = test_state();
        let msg = r#"{"jsonrpc":"2.0","id":4,"method":"unknown/method"}"#;
        let resp = handle_jsonrpc_message(&state, msg).await.unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn handle_notification_initialized_returns_none() {
        let state = test_state();
        let msg = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let resp = handle_jsonrpc_message(&state, msg).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn handle_notification_cancelled_returns_none() {
        let state = test_state();
        let msg = r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#;
        let resp = handle_jsonrpc_message(&state, msg).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn handle_parse_error() {
        let state = test_state();
        let msg = "not valid json {{{";
        let resp = handle_jsonrpc_message(&state, msg).await.unwrap();
        assert_eq!(resp["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn handle_tools_call_unknown_tool() {
        let state = test_state();
        let msg = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"bad","arguments":{}}}"#;
        let resp = handle_jsonrpc_message(&state, msg).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
    }

    #[tokio::test]
    async fn handle_tools_call_missing_params() {
        let state = test_state();
        let msg = r#"{"jsonrpc":"2.0","id":6,"method":"tools/call"}"#;
        let resp = handle_jsonrpc_message(&state, msg).await.unwrap();
        // Missing params → empty name → unknown tool
        assert_eq!(resp["result"]["isError"], true);
    }

    #[tokio::test]
    async fn handle_string_id_preserved() {
        let state = test_state();
        let msg = r#"{"jsonrpc":"2.0","id":"req-99","method":"ping"}"#;
        let resp = handle_jsonrpc_message(&state, msg).await.unwrap();
        assert_eq!(resp["id"], "req-99");
    }

    #[tokio::test]
    async fn handle_null_id_for_non_notification() {
        let state = test_state();
        let msg = r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#;
        let resp = handle_jsonrpc_message(&state, msg).await.unwrap();
        assert!(resp["id"].is_null());
    }

    #[tokio::test]
    async fn handle_channel_notification_returns_none() {
        let state = test_state();
        let msg =
            r#"{"jsonrpc":"2.0","method":"notifications/claude/channel","params":{"data":"test"}}"#;
        let resp = handle_jsonrpc_message(&state, msg).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn handle_permission_notification_returns_none() {
        let state = test_state();
        let msg = r#"{"jsonrpc":"2.0","method":"notifications/claude/channel/permission","params":{"id":"p1"}}"#;
        let resp = handle_jsonrpc_message(&state, msg).await;
        assert!(resp.is_none());
    }
}
