//! Acceptance: typed HTTP table pagination (page, offset, cursor, row cursor,
//! body cursor, link header), `dict_entries` reshape across pages, and
//! `max_pages` safety enforcement.

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
use wiremock::matchers::{body_partial_json, method, path, query_param};
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
async fn offset_pagination_combines_pages() {
    let server = MockServer::start().await;
    // size=2: offset=0 → 2 rows, offset=2 → 2 rows, offset=4 → 1 row (short) → stop.
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "fact": "a", "length": 1 }, { "fact": "b", "length": 2 }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("offset", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "fact": "c", "length": 3 }, { "fact": "d", "length": 4 }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("offset", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "fact": "e", "length": 5 }
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
            "pagination": { "type": "offset", "param": "offset", "size_param": "limit", "size": 2 }
        }),
        None,
    );

    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let batches = ctx
        .sql("SELECT fact FROM cats.facts")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 5, "offset 0 (2) + 2 (2) + 4 (1, short → stop)");
}

#[tokio::test]
async fn row_cursor_pagination_follows_last_id() {
    let server = MockServer::start().await;
    // Stripe-style: send `starting_after=<last id>`. Stop on an empty page.
    // Mount most specific first so the no-cursor first page falls through last.
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("starting_after", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("starting_after", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 3, "fact": "c" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "fact": "a" }, { "id": 2, "fact": "b" }
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
                    { "name": "id",   "type": "bigint" },
                    { "name": "fact", "type": "varchar" }
                ]
            },
            "pagination": { "type": "row_cursor", "param": "starting_after", "field": "id" }
        }),
        None,
    );

    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let batches = ctx
        .sql("SELECT id FROM cats.facts")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        rows, 3,
        "page 1 (id 1,2) + starting_after=2 (id 3) + empty → stop"
    );
}

#[tokio::test]
async fn body_cursor_pagination_injects_into_body() {
    let server = MockServer::start().await;
    // GraphQL/Notion-style: the next cursor is written into the request JSON body
    // at `variables.after`. Second page carries it; its response omits the cursor.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_partial_json(
            json!({ "variables": { "after": "CUR" } }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [ { "id": 3 } ],
            "meta": { "end": "" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [ { "id": 1 }, { "id": 2 } ],
            "meta": { "end": "CUR" }
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = facts_def(
        server.uri(),
        json!({
            "endpoint": "/graphql",
            "method": "POST",
            "body": { "kind": "json", "template": "{\"variables\": {\"first\": 2}}" },
            "response": {
                "path": "$.data",
                "schema": [ { "name": "id", "type": "bigint" } ]
            },
            "pagination": {
                "type": "body_cursor",
                "cursor_path": "$.variables.after",
                "next_path": "$.meta.end"
            }
        }),
        None,
    );

    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let batches = ctx
        .sql("SELECT id FROM cats.facts")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 3, "page 1 (2 rows) + body-cursor page (1 row)");
}

#[tokio::test]
async fn reshape_dict_entries_paginates_until_empty() {
    let server = MockServer::start().await;
    // Each page is an object map needing `dict_entries`; pagination's empty-page
    // stop must look at post-reshape rows. Page 1 → 2 rows, page 2 → {} → stop.
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "a": { "length": 1 },
            "b": { "length": 2 }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = facts_def(
        server.uri(),
        json!({
            "endpoint": "/facts",
            "response": {
                "path": "$",
                "reshape": { "kind": "dict_entries" },
                "schema": [
                    { "name": "_key",   "type": "varchar" },
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

    let batches = ctx
        .sql("SELECT _key FROM cats.facts")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 2, "reshaped page 1 (2 entries); page 2 empty → stop");
}

#[tokio::test]
async fn body_cursor_pagination_nested_path() {
    let server = MockServer::start().await;
    // GraphQL connection: rows + cursor live deep under $.data.search, not $.data.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_partial_json(
            json!({ "variables": { "after": "CUR" } }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "search": {
                "nodes": [ { "id": 3 } ],
                "pageInfo": { "endCursor": "" }
            } }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "search": {
                "nodes": [ { "id": 1 }, { "id": 2 } ],
                "pageInfo": { "endCursor": "CUR" }
            } }
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = facts_def(
        server.uri(),
        json!({
            "endpoint": "/graphql",
            "method": "POST",
            "body": { "kind": "json", "template": "{\"variables\": {\"first\": 2}}" },
            "response": {
                "path": "$.data.search.nodes",
                "schema": [ { "name": "id", "type": "bigint" } ]
            },
            "pagination": {
                "type": "body_cursor",
                "cursor_path": "$.variables.after",
                "next_path": "$.data.search.pageInfo.endCursor"
            }
        }),
        None,
    );

    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let batches = ctx
        .sql("SELECT id FROM cats.facts")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 3, "nested nodes page 1 (2) + cursor page (1)");
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
