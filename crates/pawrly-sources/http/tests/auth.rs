//! Acceptance: HTTP auth shapes — `header` (bearer + value), `basic`, and
//! `custom` (query-string credentials). Each mock only responds when the
//! credential is attached, so a successful query proves auth was applied.

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
use wiremock::matchers::{header, method, path, query_param};
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

/// Register an `items` table over `server` with the given source-level `auth`
/// config, run a query, and return the row count (so the caller asserts the
/// mock matched).
async fn rows_with_auth(server: &MockServer, auth: serde_json::Value) -> usize {
    let (ctx, catalog) = build_ctx().await;
    let mut config = json!({ "base_url": server.uri() });
    if !auth.is_null() {
        config.as_object_mut().unwrap().insert("auth".into(), auth);
    }
    let def = SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        config,
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "items".into(),
            description: None,
            config: json!({
                "endpoint": "/items",
                "response": { "path": "$", "schema": [ { "name": "id", "type": "bigint" } ] }
            }),
            cache: None,
            safety: None,
        }],
        raw_table: false,
        raw_table_safety: None,
    };
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");
    let batches = ctx
        .sql("SELECT id FROM api.items")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute: auth should make the mock match");
    batches.iter().map(|b| b.num_rows()).sum()
}

/// `header` auth with `bearer:` sends `Authorization: Bearer <token>`.
#[tokio::test]
async fn header_bearer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("authorization", "Bearer tkn-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;

    let rows = rows_with_auth(
        &server,
        json!({ "type": "header", "headers": [ { "name": "Authorization", "bearer": "tkn-123" } ] }),
    )
    .await;
    assert_eq!(rows, 1);
}

/// `header` auth with `value:` sends the literal value (API key in a header).
#[tokio::test]
async fn header_value_api_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("x-api-key", "secret-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;

    let rows = rows_with_auth(
        &server,
        json!({ "type": "header", "headers": [ { "name": "X-Api-Key", "value": "secret-key" } ] }),
    )
    .await;
    assert_eq!(rows, 1);
}

/// Multiple headers (e.g. Datadog-style API key + app key) are all attached.
#[tokio::test]
async fn header_multiple() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("dd-api-key", "k1"))
        .and(header("dd-application-key", "k2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;

    let rows = rows_with_auth(
        &server,
        json!({ "type": "header", "headers": [
            { "name": "DD-API-KEY", "value": "k1" },
            { "name": "DD-APPLICATION-KEY", "value": "k2" }
        ] }),
    )
    .await;
    assert_eq!(rows, 1);
}

/// `basic` auth base64-encodes `user:pass` into `Authorization: Basic …`.
#[tokio::test]
async fn basic_auth() {
    let server = MockServer::start().await;
    // base64("user:pass") = dXNlcjpwYXNz
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("authorization", "Basic dXNlcjpwYXNz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;

    let rows = rows_with_auth(
        &server,
        json!({ "type": "basic", "username": "user", "password": "pass" }),
    )
    .await;
    assert_eq!(rows, 1);
}

/// `custom` auth appends credentials to the query string (`?api_key=…`).
#[tokio::test]
async fn custom_query_param() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("api_key", "qk-9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;

    let rows = rows_with_auth(
        &server,
        json!({ "type": "custom", "query": [ { "name": "api_key", "value": "qk-9" } ] }),
    )
    .await;
    assert_eq!(rows, 1);
}

/// The `config.token` shorthand is a single bearer header.
#[tokio::test]
async fn token_shorthand_is_bearer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("authorization", "Bearer short-tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        config: json!({ "base_url": server.uri(), "token": "short-tok" }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "items".into(),
            description: None,
            config: json!({
                "endpoint": "/items",
                "response": { "path": "$", "schema": [ { "name": "id", "type": "bigint" } ] }
            }),
            cache: None,
            safety: None,
        }],
        raw_table: false,
        raw_table_safety: None,
    };
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");
    let rows: usize = ctx
        .sql("SELECT id FROM api.items")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute")
        .iter()
        .map(|b| b.num_rows())
        .sum();
    assert_eq!(rows, 1);
}
