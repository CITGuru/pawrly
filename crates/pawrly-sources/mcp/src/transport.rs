//! The transport seam: send a JSON-RPC request and await its result, or fire a
//! notification. `StdioTransport` and `HttpTransport` implement this; the
//! `McpClientSession` protocol logic sits above it, transport-agnostic.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::McpError;

#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a JSON-RPC request and return its `result` (mapping a JSON-RPC
    /// `error` to [`McpError::Rpc`]).
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError>;

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError>;
}

pub(crate) fn rpc_request(id: i64, method: &str, params: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub(crate) fn rpc_notification(method: &str, params: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// Pull `result` out of a JSON-RPC response envelope, surfacing `error`.
pub(crate) fn rpc_result(response: &Value) -> Result<Value, McpError> {
    if let Some(error) = response.get("error") {
        return Err(McpError::Rpc {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string(),
        });
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use std::sync::Mutex;

    type Responder = dyn Fn(&str, &Value) -> Result<Value, McpError> + Send + Sync;

    /// A transport driven by a closure `(method, params) -> result`, recording
    /// every call for assertions.
    pub(crate) struct MockTransport {
        responder: Box<Responder>,
        pub calls: Mutex<Vec<(String, Value)>>,
    }

    impl MockTransport {
        pub fn new<F>(responder: F) -> Self
        where
            F: Fn(&str, &Value) -> Result<Value, McpError> + Send + Sync + 'static,
        {
            Self {
                responder: Box::new(responder),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl McpTransport for MockTransport {
        async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_string(), params.clone()));
            (self.responder)(method, &params)
        }

        async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_string(), params));
            Ok(())
        }
    }
}
