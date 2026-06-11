//! The MCP client protocol over a [`McpTransport`]: handshake, tool discovery,
//! and tool invocation. The transport owns wire framing and the connection's
//! lifetime; this layer owns the protocol.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::error::McpError;
use crate::transport::McpTransport;

const PROTOCOL_VERSION: &str = "2025-06-18";

/// One tool advertised by `tools/list`.
#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    /// `annotations.readOnlyHint`, when present.
    pub read_only: Option<bool>,
    /// `annotations.destructiveHint`, when present.
    pub destructive: Option<bool>,
}

/// A connected MCP session. Holds the transport; dropping it tears the
/// connection down (the transport's own `Drop`).
pub struct McpClientSession {
    transport: Arc<dyn McpTransport>,
}

impl McpClientSession {
    /// Run the `initialize` handshake and return a ready session.
    pub async fn connect(transport: Arc<dyn McpTransport>) -> Result<Self, McpError> {
        let session = Self { transport };
        session.initialize().await?;
        Ok(session)
    }

    async fn initialize(&self) -> Result<(), McpError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "pawrly", "version": env!("CARGO_PKG_VERSION") },
        });
        self.transport.request("initialize", params).await?;
        self.transport
            .notify("notifications/initialized", json!({}))
            .await
    }

    /// Enumerate every tool, following `nextCursor` pages.
    pub async fn list_tools(&self) -> Result<Vec<Tool>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self.transport.request("tools/list", params).await?;
            if let Some(arr) = result.get("tools").and_then(Value::as_array) {
                tools.extend(arr.iter().filter_map(parse_tool));
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
                _ => break,
            }
        }
        Ok(tools)
    }

    /// Invoke a tool with the given arguments, returning the raw `tools/call`
    /// result. A result flagged `isError` becomes an [`McpError`].
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpError> {
        let result = self
            .transport
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(McpError::Rpc {
                code: 0,
                message: tool_error_text(&result),
            });
        }
        Ok(result)
    }
}

fn parse_tool(value: &Value) -> Option<Tool> {
    let name = value.get("name")?.as_str()?.to_string();
    let annotations = value.get("annotations");
    let hint = |key: &str| {
        annotations
            .and_then(|a| a.get(key))
            .and_then(Value::as_bool)
    };
    Some(Tool {
        name,
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        input_schema: value
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({})),
        output_schema: value.get("outputSchema").cloned(),
        read_only: hint("readOnlyHint"),
        destructive: hint("destructiveHint"),
    })
}

/// Best-effort error text from a `tools/call` result flagged `isError`.
fn tool_error_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("tool returned an error")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::MockTransport;

    #[tokio::test]
    async fn connect_handshakes_then_lists_paginated_tools() {
        let transport = Arc::new(MockTransport::new(|method, params| match method {
            "initialize" => Ok(json!({ "protocolVersion": "2025-06-18", "capabilities": {} })),
            "tools/list" => {
                // Page 1 has a cursor; page 2 does not.
                if params.get("cursor").is_none() {
                    Ok(json!({
                        "tools": [{
                            "name": "search",
                            "description": "Search",
                            "inputSchema": { "type": "object", "properties": { "q": { "type": "string" } } },
                            "annotations": { "readOnlyHint": true }
                        }],
                        "nextCursor": "p2"
                    }))
                } else {
                    Ok(json!({ "tools": [{ "name": "get_thing" }] }))
                }
            }
            other => panic!("unexpected method {other}"),
        }));

        let session = McpClientSession::connect(transport).await.expect("connect");
        let tools = session.list_tools().await.expect("list");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[0].read_only, Some(true));
        assert_eq!(tools[1].name, "get_thing");
    }

    #[tokio::test]
    async fn call_tool_maps_is_error() {
        let transport = Arc::new(MockTransport::new(|method, _| match method {
            "initialize" => Ok(json!({})),
            "tools/call" => Ok(json!({
                "isError": true,
                "content": [{ "type": "text", "text": "boom" }]
            })),
            _ => Ok(json!({})),
        }));
        let session = McpClientSession::connect(transport).await.unwrap();
        let err = session.call_tool("x", json!({})).await.unwrap_err();
        assert!(matches!(err, McpError::Rpc { ref message, .. } if message == "boom"));
    }
}
