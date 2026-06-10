//! Acceptance: an `openapi` HTTP source fetches a spec, synthesizes tables, and
//! serves real queries — including wrapped-list rows, page pagination, and
//! `include` selection — driving requests at the spec's declared server.

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
use pawrly_sources_http::register_http_source;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
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

async fn count(ctx: &SessionContext, sql: &str) -> usize {
    let batches = ctx
        .sql(sql)
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");
    batches.iter().map(|b| b.num_rows()).sum()
}

fn openapi_source(server: &MockServer, openapi_block: Value) -> SourceDef {
    let mut config = json!({
        "type": "openapi",
        "base_url": format!("{}/openapi.json", server.uri()),
    });
    if let Value::Object(extra) = openapi_block {
        config["openapi"] = Value::Object(extra);
    }
    SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config,
        cache: CachePolicy::None,
        safety: None,
        tables: Vec::new(),
        raw_table: false,
        raw_table_safety: None,
    }
}

async fn mount_spec(server: &MockServer, spec: Value) {
    Mock::given(method("GET"))
        .and(path("/openapi.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(spec))
        .mount(server)
        .await;
}

#[tokio::test]
async fn array_response_is_queryable_end_to_end() {
    let server = MockServer::start().await;
    let spec = json!({
        "openapi": "3.0.0",
        "servers": [{ "url": server.uri() }],
        "paths": { "/items": { "get": {
            "operationId": "listItems",
            "responses": { "200": { "content": { "application/json": { "schema": {
                "type": "array",
                "items": { "type": "object", "properties": {
                    "id": { "type": "integer", "format": "int64" },
                    "name": { "type": "string" }
                }}
            }}}}}
        }}}
    });
    mount_spec(&server, spec).await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "a" },
            { "id": 2, "name": "b" }
        ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &openapi_source(&server, Value::Null),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    assert_eq!(count(&ctx, "SELECT id, name FROM api.list_items").await, 2);
}

#[tokio::test]
async fn wrapped_list_paginates_across_pages() {
    let server = MockServer::start().await;
    let spec = json!({
        "openapi": "3.0.0",
        "servers": [{ "url": server.uri() }],
        "paths": { "/items": { "get": {
            "operationId": "listItems",
            "parameters": [
                { "name": "page", "in": "query", "schema": { "type": "integer" } },
                { "name": "per_page", "in": "query", "schema": { "type": "integer" } }
            ],
            "responses": { "200": { "content": { "application/json": { "schema": {
                "type": "object",
                "properties": {
                    "data": { "type": "array", "items": { "type": "object", "properties": {
                        "id": { "type": "integer", "format": "int64" }
                    }}},
                    "has_more": { "type": "boolean" }
                }
            }}}}}
        }}}
    });
    mount_spec(&server, spec).await;
    for (page, body) in [
        ("1", json!({ "data": [ { "id": 1 } ], "has_more": true })),
        ("2", json!({ "data": [ { "id": 2 } ], "has_more": true })),
        ("3", json!({ "data": [], "has_more": false })),
    ] {
        Mock::given(method("GET"))
            .and(path("/items"))
            .and(query_param("page", page))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
    }

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &openapi_source(&server, Value::Null),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    assert_eq!(count(&ctx, "SELECT id FROM api.list_items").await, 2);
}

#[tokio::test]
async fn stripe_style_row_cursor_paginates() {
    let server = MockServer::start().await;
    let spec = json!({
        "openapi": "3.0.0",
        "servers": [{ "url": server.uri() }],
        "paths": { "/v1/charges": { "get": {
            "operationId": "GetCharges",
            "parameters": [
                { "name": "limit", "in": "query", "schema": { "type": "integer" } },
                { "name": "starting_after", "in": "query", "schema": { "type": "string" } }
            ],
            "responses": { "200": { "content": { "application/json": { "schema": {
                "type": "object",
                "properties": {
                    "data": { "type": "array", "items": { "type": "object", "properties": {
                        "id": { "type": "string" }
                    }}},
                    "has_more": { "type": "boolean" }
                }
            }}}}}
        }}}
    });
    mount_spec(&server, spec).await;
    Mock::given(method("GET"))
        .and(path("/v1/charges"))
        .and(query_param_is_missing("starting_after"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "data": [ { "id": "ch_1" } ], "has_more": true })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/charges"))
        .and(query_param("starting_after", "ch_1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "data": [ { "id": "ch_2" } ], "has_more": false })),
        )
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &openapi_source(&server, Value::Null),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    assert_eq!(count(&ctx, "SELECT id FROM api.get_charges").await, 2);
}

#[tokio::test]
async fn include_selector_limits_registered_tables() {
    let server = MockServer::start().await;
    let spec = json!({
        "openapi": "3.0.0",
        "servers": [{ "url": server.uri() }],
        "paths": {
            "/charges": { "get": { "operationId": "charges", "tags": ["Charges"],
                "responses": { "200": { "content": { "application/json": { "schema": {
                    "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" } } }
                }}}}}
            }},
            "/test_clocks": { "get": { "operationId": "testClocks", "tags": ["Test"],
                "responses": { "200": { "content": { "application/json": { "schema": {
                    "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" } } }
                }}}}}
            }}
        }
    });
    mount_spec(&server, spec).await;
    Mock::given(method("GET"))
        .and(path("/charges"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": "ch_1" } ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = openapi_source(&server, json!({ "include": { "tags": ["Charges"] } }));
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    assert_eq!(count(&ctx, "SELECT id FROM api.charges").await, 1);
    assert!(
        ctx.sql("SELECT id FROM api.test_clocks").await.is_err(),
        "excluded operation must not be registered as a table"
    );
}
