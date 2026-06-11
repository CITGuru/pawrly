//! Acceptance: an `mcp` source over streamable HTTP discovers a tool, synthesizes
//! a table, and serves a real query — filters bind to tool arguments and the
//! result's rows are projected into columns.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider};
use datafusion::execution::config::SessionConfig;
use datafusion::execution::context::SessionContext;
use pawrly_core::{CachePolicy, SourceDef, SourceKind};
use pawrly_sources_mcp::register_mcp_source;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn build_ctx() -> (SessionContext, Arc<MemoryCatalogProvider>) {
    let cfg = SessionConfig::new()
        .with_default_catalog_and_schema("pawrly", "default")
        .with_create_default_catalog_and_schema(false);
    let ctx = SessionContext::new_with_config(cfg);
    let catalog: Arc<MemoryCatalogProvider> = Arc::new(MemoryCatalogProvider::new());
    let default_schema: Arc<dyn datafusion::catalog::SchemaProvider> =
        Arc::new(datafusion::catalog::MemorySchemaProvider::new());
    let _ = catalog.register_schema("default", default_schema).unwrap();
    ctx.register_catalog("pawrly", catalog.clone());
    (ctx, catalog)
}

async fn collect(ctx: &SessionContext, sql: &str) -> Vec<arrow_array::RecordBatch> {
    ctx.sql(sql)
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute")
}

/// Mount a JSON-RPC method on the mock server.
async fn mount(server: &MockServer, method_name: &str, result: serde_json::Value, status: u16) {
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
async fn streamable_http_tool_is_queryable() {
    let server = MockServer::start().await;
    mount(
        &server,
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {} }),
        200,
    )
    .await;
    mount(&server, "notifications/initialized", json!({}), 202).await;
    mount(
        &server,
        "tools/list",
        json!({
            "tools": [{
                "name": "search_issues",
                "description": "Search issues",
                "inputSchema": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                },
                "outputSchema": {
                    "type": "object",
                    "properties": {
                        "issues": { "type": "array", "items": { "type": "object", "properties": {
                            "id": { "type": "string" },
                            "title": { "type": "string" }
                        }}}
                    }
                },
                "annotations": { "readOnlyHint": true }
            }]
        }),
        200,
    )
    .await;
    mount(
        &server,
        "tools/call",
        json!({
            "structuredContent": {
                "issues": [
                    { "id": "ISS-1", "title": "first" },
                    { "id": "ISS-2", "title": "second" }
                ]
            }
        }),
        200,
    )
    .await;

    let def = SourceDef {
        name: "linear".into(),
        kind: SourceKind::Mcp,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({ "transport": "streamable_http", "url": server.uri() }),
        cache: CachePolicy::None,
        safety: None,
        tables: Vec::new(),
        raw_table: false,
        raw_table_safety: None,
    };

    let (ctx, catalog) = build_ctx().await;
    let report = register_mcp_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");
    assert_eq!(report.table_count, 1);

    let batches = collect(
        &ctx,
        "SELECT id, title FROM linear.search_issues WHERE query = 'bug' ORDER BY id",
    )
    .await;
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 2);

    // The output columns were inferred from `outputSchema`; `query` is also a column (echoed arg).
    let with_arg = collect(
        &ctx,
        "SELECT query, id FROM linear.search_issues WHERE query = 'bug'",
    )
    .await;
    assert_eq!(with_arg.iter().map(|b| b.num_rows()).sum::<usize>(), 2);
}

/// Hits the live Linear MCP server. Set `PAWRLY_LINEAR_TOKEN`; run with `--ignored`.
#[tokio::test]
#[ignore = "network: set PAWRLY_LINEAR_TOKEN to reach the live Linear MCP server"]
async fn linear_streamable_http_lists_tools() {
    let token = std::env::var("PAWRLY_LINEAR_TOKEN").expect("set PAWRLY_LINEAR_TOKEN");
    let def = SourceDef {
        name: "linear".into(),
        kind: SourceKind::Mcp,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({
            "transport": "streamable_http",
            "url": "https://mcp.linear.app/mcp",
            "auth": { "type": "header", "headers": [{ "name": "Authorization", "bearer": token }] }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: Vec::new(),
        raw_table: false,
        raw_table_safety: None,
    };

    let (_ctx, catalog) = build_ctx().await;
    let report = register_mcp_source(&def, &_ctx, catalog.as_ref())
        .await
        .expect("register");
    eprintln!("linear: {} tables", report.table_count);
    for t in &report.tables {
        eprintln!("  {}", t.name);
    }
    assert!(report.table_count > 0);
}
