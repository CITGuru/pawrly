//! Transport-agnostic JSON-RPC 2.0 dispatch for MCP. Both the stdio and HTTP
//! transports parse a request into a [`Value`] and hand it here.

use std::sync::Arc;

use pawrly_core::EngineService;
use serde_json::{Value, json};

use crate::tools::{ToolError, call_tool, list_tools};

/// Protocol revision this server implements.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Handle one parsed JSON-RPC message. Returns `None` for notifications (a
/// message with no `id`), which by spec receive no response.
pub async fn handle_message(engine: &Arc<dyn EngineService>, req: &Value) -> Option<Value> {
    // Notifications carry no `id` and get no reply.
    let id = req.get("id").cloned()?;
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

    Some(match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_VERSION)
                .to_string();
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "pawrly-mcp",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            })
        }
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": list_tools() }
        }),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(engine, &name, &args).await {
                Ok(v) => tool_result(id, v),
                // Engine failures are tool-execution errors: reported in-band so
                // the model can read them, not as protocol-level errors.
                Err(ToolError::Engine(msg)) => tool_error(id, &msg),
                // Bad arguments / unknown tool are protocol-level errors.
                Err(e) => error_response(id, -32602, &e.to_string()),
            }
        }
        other => error_response(id, -32601, &format!("unknown method: {other}")),
    })
}

/// Wrap a successful tool payload in the MCP `tools/call` result envelope.
fn tool_result(id: Value, payload: Value) -> Value {
    let text = serde_json::to_string(&payload).unwrap_or_default();
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }],
            "structuredContent": payload,
            "isError": false
        }
    })
}

/// A tool-execution error: an MCP result with `isError: true`.
fn tool_error(id: Value, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": message }],
            "isError": true
        }
    })
}

/// A JSON-RPC protocol-level error.
pub fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_core::test_support::MockEngine;

    fn engine() -> Arc<dyn EngineService> {
        Arc::new(MockEngine::new())
    }

    #[tokio::test]
    async fn notification_gets_no_response() {
        let req = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle_message(&engine(), &req).await.is_none());
    }

    #[tokio::test]
    async fn ping_returns_empty_result() {
        let req = json!({ "jsonrpc": "2.0", "id": 7, "method": "ping" });
        let resp = handle_message(&engine(), &req).await.unwrap();
        assert_eq!(resp["id"], 7);
        assert_eq!(resp["result"], json!({}));
    }

    #[tokio::test]
    async fn initialize_echoes_requested_version() {
        let req = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2024-11-05" }
        });
        let resp = handle_message(&engine(), &req).await.unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["serverInfo"].is_object());
    }

    #[tokio::test]
    async fn initialize_defaults_version_when_absent() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" });
        let resp = handle_message(&engine(), &req).await.unwrap();
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn tools_call_wraps_in_content_envelope() {
        let req = json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "list_tables", "arguments": {} }
        });
        let resp = handle_message(&engine(), &req).await.unwrap();
        let result = &resp["result"];
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["type"], "text");
        assert!(result["structuredContent"]["tables"].is_array());
    }

    #[tokio::test]
    async fn tool_engine_error_is_in_band() {
        // `describe_table` for an unknown table is an engine error.
        let req = json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "describe_table", "arguments": { "table": "gh.nope" } }
        });
        let resp = handle_message(&engine(), &req).await.unwrap();
        assert!(resp.get("error").is_none());
        assert_eq!(resp["result"]["isError"], true);
    }

    #[tokio::test]
    async fn bad_arguments_are_protocol_error() {
        let req = json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "describe_table", "arguments": {} }
        });
        let resp = handle_message(&engine(), &req).await.unwrap();
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn unknown_method_is_protocol_error() {
        let req = json!({ "jsonrpc": "2.0", "id": 6, "method": "frobnicate" });
        let resp = handle_message(&engine(), &req).await.unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }
}
