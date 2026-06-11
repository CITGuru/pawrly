//! Streamable-HTTP transport: POST JSON-RPC to the server, reading either a
//! single JSON response or an SSE stream, and carry the `Mcp-Session-Id`.

use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::McpError;
use crate::transport::{McpTransport, rpc_notification, rpc_request, rpc_result};

const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_VERSION: &str = "2025-06-18";

pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    auth: HeaderMap,
    session_id: Mutex<Option<String>>,
    next_id: AtomicI64,
}

impl HttpTransport {
    pub fn new(url: String, auth: HeaderMap) -> Self {
        Self {
            client: reqwest::Client::new(),
            url,
            auth,
            session_id: Mutex::new(None),
            next_id: AtomicI64::new(1),
        }
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response, McpError> {
        let mut req = self
            .client
            .post(&self.url)
            .headers(self.auth.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION);
        if let Some(id) = self.session_id.lock().await.as_ref() {
            req = req.header(SESSION_HEADER, id);
        }
        let resp = req
            .json(body)
            .send()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;

        if let Some(id) = resp
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().await = Some(id.to_string());
        }
        if !resp.status().is_success() {
            return Err(McpError::Transport(format!("HTTP {}", resp.status())));
        }
        Ok(resp)
    }
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let resp = self.post(&rpc_request(id, method, &params)).await?;

        let is_sse = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/event-stream"));
        let body = resp
            .text()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;

        let response = if is_sse {
            sse_response_for(&body, id).ok_or_else(|| {
                McpError::Protocol("no matching JSON-RPC message in SSE stream".into())
            })?
        } else {
            serde_json::from_str(&body).map_err(|e| McpError::Protocol(e.to_string()))?
        };
        rpc_result(&response)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        self.post(&rpc_notification(method, &params)).await?;
        Ok(())
    }
}

/// Parse an SSE body and return the JSON-RPC message whose `id` matches.
fn sse_response_for(body: &str, id: i64) -> Option<Value> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .find(|msg| msg.get("id").and_then(Value::as_i64) == Some(id))
}

/// Build request headers from a `config.auth` block (reusing the HTTP source's
/// `header`/`bearer`/`basic` shapes). Credentials are expected pre-resolved
/// (the config layer interpolates `${secret:…}`).
pub fn auth_headers(auth: &Value) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let insert = |headers: &mut HeaderMap, name: &str, value: String| {
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            headers.insert(name, value);
        }
    };
    match auth.get("type").and_then(Value::as_str) {
        Some("header") => {
            for header in auth
                .get("headers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = header.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if let Some(bearer) = header.get("bearer").and_then(Value::as_str) {
                    insert(&mut headers, name, format!("Bearer {bearer}"));
                } else if let Some(value) = header.get("value").and_then(Value::as_str) {
                    insert(&mut headers, name, value.to_string());
                }
            }
        }
        Some("bearer") => {
            if let Some(token) = auth.get("token").and_then(Value::as_str) {
                insert(&mut headers, "Authorization", format!("Bearer {token}"));
            }
        }
        _ => {}
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sse_picks_matching_id() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let msg = sse_response_for(body, 1).unwrap();
        assert_eq!(msg["result"]["ok"], json!(true));
        assert!(sse_response_for(body, 2).is_none());
    }

    #[test]
    fn auth_headers_build_bearer() {
        let headers = auth_headers(&json!({
            "type": "header",
            "headers": [{ "name": "Authorization", "bearer": "tok" }]
        }));
        assert_eq!(headers.get("Authorization").unwrap(), "Bearer tok");

        let headers = auth_headers(&json!({ "type": "bearer", "token": "tok2" }));
        assert_eq!(headers.get("Authorization").unwrap(), "Bearer tok2");
    }
}
