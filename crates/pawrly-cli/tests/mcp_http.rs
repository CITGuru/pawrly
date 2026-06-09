//! Acceptance: spawn `pawrly mcp-http`, POST a JSON-RPC `initialize` over a
//! real TCP socket, and verify the response.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;
use serde_json::{Value, json};
use tempfile::TempDir;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn write_workspace(dir: &std::path::Path) -> PathBuf {
    let fx = fixtures_dir();
    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{}"
    tables:
      - name: orders
        path: "{}"
        format: parquet
"#,
        fx.display(),
        fx.join("orders.parquet").display(),
    );
    let path = dir.join("pawrly.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

/// Grab an ephemeral port by binding then immediately releasing it.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// POST one JSON-RPC message to `/mcp`, returning the parsed response body.
/// Retries the connect until `deadline`, so the test tolerates server startup.
fn post_mcp(port: u16, msg: &Value, deadline: Instant) -> Value {
    let body = serde_json::to_vec(msg).unwrap();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );

    loop {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            stream.write_all(request.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).unwrap();
            let text = String::from_utf8_lossy(&raw);
            let payload = text.split("\r\n\r\n").nth(1).expect("response body");
            return serde_json::from_str(payload).expect("json body");
        }
        assert!(Instant::now() < deadline, "server never came up");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn mcp_http_initialize_round_trip() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_workspace(tmp.path());
    let port = free_port();

    let mut child = Command::new(cargo_bin("pawrly"))
        .args([
            "--no-remote",
            "--config",
            cfg.to_str().unwrap(),
            "mcp-http",
            "--addr",
            &format!("127.0.0.1:{port}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp-http");

    let deadline = Instant::now() + Duration::from_secs(10);
    let init = post_mcp(
        port,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
        deadline,
    );
    assert_eq!(init["id"], json!(1));
    assert!(init["result"]["serverInfo"].is_object());

    let call = post_mcp(
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "query",
                "arguments": { "sql": "SELECT COUNT(*) AS n FROM data.orders" }
            }
        }),
        deadline,
    );
    assert_eq!(call["id"], json!(2));
    assert_eq!(call["result"]["isError"], json!(false));
    assert_eq!(call["result"]["structuredContent"]["rows"][0][0], json!("5"));

    child.kill().ok();
    child.wait().ok();
}
