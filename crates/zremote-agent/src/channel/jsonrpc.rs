/// Build a JSON-RPC 2.0 success response.
pub fn jsonrpc_ok(id: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

/// Build a JSON-RPC 2.0 error response.
pub fn jsonrpc_error(id: serde_json::Value, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

/// Build a JSON-RPC 2.0 notification (no id).
pub fn jsonrpc_notification(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_response_format() {
        let resp = jsonrpc_ok(serde_json::json!(1), serde_json::json!({"status": "ok"}));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["status"], "ok");
        assert!(resp.get("error").is_none());
    }

    #[test]
    fn ok_response_with_string_id() {
        let resp = jsonrpc_ok(serde_json::json!("req-42"), serde_json::json!({}));
        assert_eq!(resp["id"], "req-42");
    }

    #[test]
    fn ok_response_with_null_id() {
        let resp = jsonrpc_ok(serde_json::Value::Null, serde_json::json!({}));
        assert!(resp["id"].is_null());
    }

    #[test]
    fn error_response_format() {
        let resp = jsonrpc_error(serde_json::json!(2), -32601, "Method not found");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 2);
        assert_eq!(resp["error"]["code"], -32601);
        assert_eq!(resp["error"]["message"], "Method not found");
        assert!(resp.get("result").is_none());
    }

    #[test]
    fn error_response_with_parse_error_code() {
        let resp = jsonrpc_error(serde_json::Value::Null, -32700, "Parse error");
        assert_eq!(resp["error"]["code"], -32700);
    }

    #[test]
    fn notification_format() {
        let notif = jsonrpc_notification(
            "notifications/claude/channel",
            serde_json::json!({"data": 1}),
        );
        assert_eq!(notif["jsonrpc"], "2.0");
        assert_eq!(notif["method"], "notifications/claude/channel");
        assert_eq!(notif["params"]["data"], 1);
        assert!(notif.get("id").is_none());
    }

    #[test]
    fn notification_with_empty_params() {
        let notif = jsonrpc_notification("ping", serde_json::json!({}));
        assert_eq!(notif["method"], "ping");
        assert!(notif["params"].is_object());
    }
}
