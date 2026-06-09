//! Acceptance: spawn `pawrly mcp-stdio`, send JSON-RPC frames over stdin,
//! verify `tools/list` and `tools/call query` round-trip.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use assert_cmd::cargo::cargo_bin;
use serde_json::{Value, json};
use tempfile::TempDir;

fn pawrly() -> Command {
    Command::new(cargo_bin("pawrly"))
}

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

#[test]
fn mcp_stdio_query_round_trip() {
    let tmp = TempDir::new().unwrap();
    let cfg = write_workspace(tmp.path());

    let mut child = pawrly()
        .args([
            "--no-remote",
            "--config",
            cfg.to_str().unwrap(),
            "mcp-stdio",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp-stdio");

    // initialize
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
    // tools/list
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    // tools/call query
    let call = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "query",
            "arguments": {"sql": "SELECT COUNT(*) AS n FROM data.orders"}
        }
    });

    {
        let stdin = child.stdin.as_mut().unwrap();
        for msg in [&init, &list, &call] {
            let mut bytes = serde_json::to_vec(msg).unwrap();
            bytes.push(b'\n');
            stdin.write_all(&bytes).unwrap();
        }
    }
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    // Three responses, in order, matched by id.
    assert!(lines.len() >= 3, "expected ≥3 responses, got {stdout}");

    let parse = |s: &str| -> Value { serde_json::from_str(s).unwrap_or(Value::Null) };
    let r1 = parse(lines[0]);
    let r2 = parse(lines[1]);
    let r3 = parse(lines[2]);

    assert_eq!(r1["id"], json!(1));
    assert!(r1["result"]["serverInfo"].is_object());

    assert_eq!(r2["id"], json!(2));
    let tools = r2["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "query"));
    assert!(tools.iter().any(|t| t["name"] == "list_tables"));

    assert_eq!(r3["id"], json!(3));
    let result = &r3["result"];
    assert_eq!(result["isError"], json!(false));
    let payload = &result["structuredContent"];
    assert_eq!(payload["columns"], json!(["n"]));
    let rows = payload["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], json!("5"));
}
