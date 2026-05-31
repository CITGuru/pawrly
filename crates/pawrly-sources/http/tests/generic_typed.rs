//! Acceptance: a generic `kind: http` source with user-declared `tables:`
//! (endpoint + response spec read from each table's opaque `config`).

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

/// A generic `http` source can declare typed tables under `tables:`, with the
/// per-table `endpoint` + `response` living in the table's `config` body, and
/// the table name taken from `TableDef.name`. The rows extract from a nested
/// `response.path`.
#[tokio::test]
async fn generic_http_user_table_registers_and_queries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "fact": "Cats sleep a lot.", "length": 17 },
                { "fact": "Cats purr.",        "length": 10 }
            ]
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;

    let def = SourceDef {
        name: "cats".into(),
        kind: SourceKind::Http,
        description: None,
        config: json!({ "base_url": server.uri() }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "facts".into(),
            description: None,
            config: json!({
                "endpoint": "/facts",
                "response": {
                    "path": "$.data",
                    "schema": [
                        { "name": "fact",   "type": "varchar" },
                        { "name": "length", "type": "bigint" }
                    ]
                }
            }),
            cache: None,
            safety: None,
        }],
        raw_table: false,
        raw_table_safety: None,
    };

    let report = register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register generic http");
    assert!(
        report.tables.iter().any(|t| t.name == "facts"),
        "expected the user-declared `facts` table to register"
    );

    let df = ctx
        .sql("SELECT fact, length FROM cats.facts ORDER BY length DESC")
        .await
        .expect("plan");
    let batches = df.collect().await.expect("execute");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 2, "both rows from response.path = $.data should load");
}

/// A malformed table body (missing `endpoint`) is a clear config error, not a
/// silent no-op.
#[tokio::test]
async fn generic_http_invalid_table_errors() {
    let (ctx, catalog) = build_ctx().await;
    let def = SourceDef {
        name: "cats".into(),
        kind: SourceKind::Http,
        description: None,
        config: json!({ "base_url": "https://example.invalid" }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "facts".into(),
            description: None,
            // No `endpoint` / `response` → cannot build an HttpTableSpec.
            config: json!({ "oops": true }),
            cache: None,
            safety: None,
        }],
        raw_table: false,
        raw_table_safety: None,
    };

    let err = register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect_err("missing endpoint should fail registration");
    let s = err.to_string();
    assert!(
        s.contains("facts"),
        "error should name the offending table: {s}"
    );
}
