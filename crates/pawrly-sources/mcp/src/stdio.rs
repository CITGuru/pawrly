//! stdio transport: spawn the MCP server as a child process and exchange
//! line-delimited JSON-RPC over its stdin/stdout, correlating responses by id.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use crate::error::McpError;
use crate::transport::{McpTransport, rpc_notification, rpc_request, rpc_result};

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

pub struct StdioTransport {
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicI64,
    reader: Mutex<Option<JoinHandle<()>>>,
    // Held so the child stays alive; `kill_on_drop` tears it down with us.
    _child: Mutex<Child>,
}

impl StdioTransport {
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<Self, McpError> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .envs(env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| McpError::Transport(format!("spawn `{command}`: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("child has no stdout".into()))?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader = {
            let pending = pending.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let Ok(message) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if let Some(id) = message.get("id").and_then(Value::as_i64)
                        && let Some(tx) = pending.lock().await.remove(&id)
                    {
                        let _ = tx.send(message);
                    }
                }
            })
        };

        Ok(Self {
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicI64::new(1),
            reader: Mutex::new(Some(reader)),
            _child: Mutex::new(child),
        })
    }

    async fn write(&self, value: &Value) -> Result<(), McpError> {
        let mut bytes = serde_json::to_vec(value).map_err(|e| McpError::Protocol(e.to_string()))?;
        bytes.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.write(&rpc_request(id, method, &params)).await?;
        let response = rx
            .await
            .map_err(|_| McpError::Transport("server closed the connection".into()))?;
        rpc_result(&response)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        self.write(&rpc_notification(method, &params)).await
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        if let Some(reader) = self.reader.get_mut().take() {
            reader.abort();
        }
    }
}
