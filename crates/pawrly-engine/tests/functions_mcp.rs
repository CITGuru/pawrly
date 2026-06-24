//! Acceptance: an attached `mcp` table-valued function end-to-end through the
//! engine, over streamable HTTP against a wiremock JSON-RPC responder. Verifies
//! the call arg arrives as a tool argument (via its `tool_arg` wire name), rows
//! map to the declared `returns`, and `source: arg` echoes the bound argument.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use pawrly_config::Config;
use pawrly_core::{EngineService, EngineServiceExt};
use pawrly_engine::{LocalEngine, LocalEngineConfig};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg_yaml(yaml: &str) -> Config {
    let secrets = pawrly_secrets::StaticStore::new();
    pawrly_config::load_str(yaml, &secrets).expect("config parse")
}

fn string_col(batch: &arrow_array::RecordBatch, name: &str) -> Vec<String> {
    use arrow_array::Array;
    let idx = batch.schema().index_of(name).unwrap();
    let arr = batch
        .column(idx)
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    (0..arr.len()).map(|i| arr.value(i).to_string()).collect()
}

/// Mount a JSON-RPC method on the mock server with an optional extra body match.
async fn mount_method(
    server: &MockServer,
    method_name: &str,
    result: serde_json::Value,
    status: u16,
) {
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": method_name })))
        .respond_with(
            ResponseTemplate::new(status)
                .set_body_json(json!({ "jsonrpc": "2.0", "id": 1, "result": result })),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn attached_mcp_function_end_to_end() {
    let server = MockServer::start().await;
    mount_method(
        &server,
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {} }),
        200,
    )
    .await;
    mount_method(&server, "notifications/initialized", json!({}), 202).await;
    // Discovery: one read-only tool `search`.
    mount_method(
        &server,
        "tools/list",
        json!({
            "tools": [{
                "name": "search",
                "description": "Search issues",
                "inputSchema": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                },
                "annotations": { "readOnlyHint": true }
            }]
        }),
        200,
    )
    .await;
    // The tools/call must carry the function's `q` arg under its `tool_arg`
    // wire name `query` — this match asserts the binding reached the tool.
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "method": "tools/call",
            "params": { "arguments": { "query": "is:open" } }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "structuredContent": {
                    "issues": [
                        { "id": "ENG-1", "title": "first" },
                        { "id": "ENG-2", "title": "second" }
                    ]
                }
            }
        })))
        .mount(&server)
        .await;

    let yaml = format!(
        r#"version: 1
sources:
  - name: linear
    kind: mcp
    config:
      transport: streamable_http
      url: "{}"
    functions:
      - name: find
        tool: search
        args:
          - {{ name: q, type: varchar, required: true, tool_arg: query }}
        rows_path: [issues]
        returns:
          - {{ name: id,    type: varchar }}
          - {{ name: title, type: varchar }}
          - {{ name: q,     type: varchar, source: arg }}
"#,
        server.uri()
    );

    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&yaml),
        workspace_dir: std::env::temp_dir(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    let batches = svc
        .query_collect("SELECT id, title, q FROM linear.find('is:open') ORDER BY id")
        .await
        .expect("query");
    let ids: Vec<String> = batches.iter().flat_map(|b| string_col(b, "id")).collect();
    assert_eq!(ids, vec!["ENG-1".to_string(), "ENG-2".to_string()]);
    let titles: Vec<String> = batches
        .iter()
        .flat_map(|b| string_col(b, "title"))
        .collect();
    assert_eq!(titles, vec!["first".to_string(), "second".to_string()]);
    // `source: arg` echoes the bound q.
    let qs: Vec<String> = batches.iter().flat_map(|b| string_col(b, "q")).collect();
    assert_eq!(qs, vec!["is:open".to_string(), "is:open".to_string()]);

    // WHERE applies on top of the function result.
    let batches = svc
        .query_collect("SELECT id FROM linear.find('is:open') WHERE title = 'second'")
        .await
        .expect("query");
    let ids: Vec<String> = batches.iter().flat_map(|b| string_col(b, "id")).collect();
    assert_eq!(ids, vec!["ENG-2".to_string()]);
}
