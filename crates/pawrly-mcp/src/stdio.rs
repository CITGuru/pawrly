//! Stdio JSON-RPC 2.0 transport for MCP. Each request/response is one JSON
//! object per line on stdin / stdout. Logs go to stderr.

use std::sync::Arc;

use pawrly_core::EngineService;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use crate::dispatch::{error_response, handle_message};

/// Run the stdio MCP server until stdin closes.
pub async fn serve_stdio(engine: Arc<dyn EngineService>) -> std::io::Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(req) => handle_message(&engine, &req).await,
            Err(e) => Some(error_response(
                Value::Null,
                -32700,
                &format!("parse error: {e}"),
            )),
        };
        let Some(response) = response else {
            continue;
        };
        let mut bytes = serde_json::to_vec(&response)?;
        bytes.push(b'\n');
        stdout.write_all(&bytes).await?;
        stdout.flush().await?;
    }
    Ok(())
}
