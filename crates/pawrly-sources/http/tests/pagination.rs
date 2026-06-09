//! Acceptance: typed HTTP table pagination (page, cursor, link header) plus
//! `max_pages` safety enforcement. Mirrors the wiremock pattern in
//! `github_typed.rs`.

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
use pawrly_core::{CachePolicy, SafetyPolicy, SourceDef, SourceKind, TableDef};
use pawrly_sources_http::register_http_source;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
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

/// A `facts` table whose `config` body the caller supplies (endpoint, response,
/// pagination), with an optional source-level safety policy.
fn facts_def(
    base_url: String,
    table_config: serde_json::Value,
    max_pages: Option<u32>,
) -> SourceDef {
    SourceDef {
        name: "cats".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({ "base_url": base_url }),
        cache: CachePolicy::None,
        safety: max_pages.map(|m| SafetyPolicy {
            max_pages: Some(m),
            ..SafetyPolicy::default()
        }),
        tables: vec![TableDef {
            name: "facts".into(),
            description: None,
            wiki: None,
            config: table_config,
            cache: None,
            safety: None,
        }],
        raw_table: false,
        raw_table_safety: None,
    }
}

#[tokio::test]
async fn page_number_pagination_combines_all_pages() {
    let server = MockServer::start().await;
    // page=1 and page=2 carry two rows each; page=3 is empty → stop.
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "fact": "a", "length": 1 },
            { "fact": "b", "length": 2 }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "fact": "c", "length": 3 },
            { "fact": "d", "length": 4 }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("page", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = facts_def(
        server.uri(),
        json!({
            "endpoint": "/facts",
            "response": {
                "path": "$",
                "schema": [
                    { "name": "fact",   "type": "varchar" },
                    { "name": "length", "type": "bigint" }
                ]
            },
            "pagination": { "type": "page", "param": "page", "start": 1 }
        }),
        None,
    );

    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let df = ctx.sql("SELECT fact FROM cats.facts").await.expect("plan");
    let batches = df.collect().await.expect("execute");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 4, "should combine page 1 + page 2 rows");
}

#[tokio::test]
async fn cursor_pagination_follows_next_cursor() {
    let server = MockServer::start().await;
    // First page (no cursor param) carries `next_cursor`; second page (cursor=c2)
    // omits it → stop.
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("cursor", "c2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [ { "fact": "c", "length": 3 } ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [ { "fact": "a", "length": 1 }, { "fact": "b", "length": 2 } ],
            "meta": { "next": "c2" }
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = facts_def(
        server.uri(),
        json!({
            "endpoint": "/facts",
            "response": {
                "path": "$.data",
                "schema": [
                    { "name": "fact",   "type": "varchar" },
                    { "name": "length", "type": "bigint" }
                ]
            },
            "pagination": { "type": "cursor", "next_path": "$.meta.next", "param": "cursor" }
        }),
        None,
    );

    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let df = ctx.sql("SELECT fact FROM cats.facts").await.expect("plan");
    let batches = df.collect().await.expect("execute");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 3, "page 1 (2 rows) + cursor page (1 row)");
}

#[tokio::test]
async fn link_header_pagination_follows_rel_next() {
    let server = MockServer::start().await;
    let next_url = format!("{}/facts?page=2", server.uri());
    // Page 1 sets a Link header to page 2; page 2 has none → stop.
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "fact": "c", "length": 3 }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Link", format!("<{next_url}>; rel=\"next\"").as_str())
                .set_body_json(json!([
                    { "fact": "a", "length": 1 },
                    { "fact": "b", "length": 2 }
                ])),
        )
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = facts_def(
        server.uri(),
        json!({
            "endpoint": "/facts",
            "response": {
                "path": "$",
                "schema": [
                    { "name": "fact",   "type": "varchar" },
                    { "name": "length", "type": "bigint" }
                ]
            },
            "pagination": { "type": "link_header" }
        }),
        None,
    );

    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let df = ctx.sql("SELECT fact FROM cats.facts").await.expect("plan");
    let batches = df.collect().await.expect("execute");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 3, "page 1 (2 rows) + linked page 2 (1 row)");
}

#[tokio::test]
async fn max_pages_enforced() {
    let server = MockServer::start().await;
    // Every page returns rows, so without a cap it would page forever.
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "fact": "a", "length": 1 }
        ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = facts_def(
        server.uri(),
        json!({
            "endpoint": "/facts",
            "response": {
                "path": "$",
                "schema": [
                    { "name": "fact",   "type": "varchar" },
                    { "name": "length", "type": "bigint" }
                ]
            },
            "pagination": { "type": "page", "param": "page", "start": 1 }
        }),
        Some(2),
    );

    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let df = ctx.sql("SELECT fact FROM cats.facts").await.expect("plan");
    let err = df.collect().await.expect_err("should hit max_pages cap");
    let s = err.to_string();
    assert!(
        s.contains("page more than 2 times") || s.contains("2"),
        "expected a TooManyPages-style error, got: {s}"
    );
}
