//! Acceptance: typed HTTP table retry behavior (5xx and 429 with Retry-After).
//! Uses wiremock scenarios so the first response fails and a later one succeeds.

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
use pawrly_core::{CachePolicy, SourceDef, SourceKind, TableDef};
use pawrly_sources_http::register_http_source;
use serde_json::json;
use wiremock::matchers::{method, path};
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

/// A single-page `facts` table with small backoff so retries are fast.
fn facts_def(base_url: String) -> SourceDef {
    SourceDef {
        name: "cats".into(),
        kind: SourceKind::Http,
        description: None,
        config: json!({
            "base_url": base_url,
            "retry": { "max_retries": 3, "base_backoff_ms": 1, "max_backoff_ms": 5 }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "facts".into(),
            description: None,
            config: json!({
                "endpoint": "/facts",
                "response": {
                    "path": "$",
                    "schema": [ { "name": "fact", "type": "varchar" } ]
                }
            }),
            cache: None,
            safety: None,
        }],
        raw_table: false,
        raw_table_safety: None,
    }
}

#[tokio::test]
async fn retries_after_503() {
    let server = MockServer::start().await;
    // First call: 503. Second call: 200.
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "fact": "a" },
            { "fact": "b" }
        ])))
        .with_priority(2)
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = facts_def(server.uri());
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let df = ctx.sql("SELECT fact FROM cats.facts").await.expect("plan");
    let batches = df.collect().await.expect("execute after retry");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 2, "should succeed on the retried 200 response");
}

#[tokio::test]
async fn retries_after_429_with_retry_after() {
    let server = MockServer::start().await;
    // First call: 429 with Retry-After: 1 (second). Second call: 200.
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "fact": "a" }
        ])))
        .with_priority(2)
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = facts_def(server.uri());
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let df = ctx.sql("SELECT fact FROM cats.facts").await.expect("plan");
    let batches = df.collect().await.expect("execute after 429 retry");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1, "should succeed after honoring Retry-After");
}
