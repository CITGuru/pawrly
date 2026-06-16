//! Acceptance: OAuth2 client-credentials (token fetch + caching) and
//! conditional requests (list vs get-by-id selected by bound filters).

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
use wiremock::matchers::{body_json, body_string_contains, header, method, path};
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

/// An OAuth2 source exchanges client credentials for a bearer token, sends it on
/// the data request, and caches it — a second query reuses the token rather than
/// re-fetching (the token endpoint only answers once).
#[tokio::test]
async fn oauth2_fetches_caches_and_authorizes() {
    let server = MockServer::start().await;
    // Token endpoint answers exactly once; a second exchange would 404.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok-abc-123",
            "expires_in": 3600
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // Data endpoint requires the issued bearer token.
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("authorization", "Bearer tok-abc-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({
            "base_url": server.uri(),
            "auth": {
                "type": "oauth2",
                "token_url": format!("{}/oauth/token", server.uri()),
                "client_id": "id",
                "client_secret": "secret"
            }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "items".into(),
            description: None,
            wiki: None,
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

    assert_eq!(count(&ctx, "SELECT id FROM api.items").await, 1);
    // Second query must reuse the cached token (token endpoint is exhausted).
    assert_eq!(count(&ctx, "SELECT id FROM api.items").await, 1);
}

/// The client-credentials exchange includes the configured `scope` and
/// `audience` in the token request — the token endpoint only answers when both
/// are present, so a successful query proves they were sent.
#[tokio::test]
async fn oauth2_sends_scope_and_audience() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("scope=read"))
        .and(body_string_contains("audience=acme"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({
            "base_url": server.uri(),
            "auth": {
                "type": "oauth2",
                "token_url": format!("{}/oauth/token", server.uri()),
                "client_id": "id",
                "client_secret": "secret",
                "scope": "read",
                "audience": "acme"
            }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "items".into(),
            description: None,
            wiki: None,
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

    assert_eq!(count(&ctx, "SELECT id FROM api.items").await, 1);
}

/// A token whose `expires_in` falls inside the refresh window is re-fetched on
/// the next query rather than reused — the token endpoint is hit twice.
#[tokio::test]
async fn oauth2_refreshes_expired_token() {
    let server = MockServer::start().await;
    // First exchange yields a token expiring within the refresh skew; the second
    // yields a long-lived one. Data endpoint accepts either bearer.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok1",
            "expires_in": 1
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok2",
            "expires_in": 3600
        })))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({
            "base_url": server.uri(),
            "auth": {
                "type": "oauth2",
                "token_url": format!("{}/oauth/token", server.uri()),
                "client_id": "id",
                "client_secret": "secret"
            }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "items".into(),
            description: None,
            wiki: None,
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

    assert_eq!(count(&ctx, "SELECT id FROM api.items").await, 1);
    assert_eq!(count(&ctx, "SELECT id FROM api.items").await, 1);

    let token_requests = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/oauth/token")
        .count();
    assert_eq!(token_requests, 2, "the expiring token should be refreshed");
}

/// A table with a conditional request uses the get-by-id endpoint when `number`
/// is bound, and the list endpoint otherwise.
#[tokio::test]
async fn conditional_request_selects_by_bound_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1 }, { "id": 2 }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 5 } ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({ "base_url": server.uri() }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "issues".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/issues",
                "params": [ { "name": "number", "type": "bigint" } ],
                "requests": [
                    { "when_filters": ["number"], "endpoint": "/issues/{number}" }
                ],
                "response": {
                    "path": "$",
                    "schema": [
                        { "name": "id",     "type": "bigint" },
                        { "name": "number", "type": "bigint", "source": "param" }
                    ]
                }
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

    // No `number` → list endpoint (2 rows).
    assert_eq!(count(&ctx, "SELECT id FROM api.issues").await, 2);
    // `number` bound → get-by-id endpoint (1 row).
    assert_eq!(
        count(&ctx, "SELECT id FROM api.issues WHERE number = 5").await,
        1
    );
}

/// A conditional request can swap method *and* body: list via GET by default,
/// but POST a search body when the `q` filter is bound.
#[tokio::test]
async fn conditional_request_swaps_method_and_body() {
    let server = MockServer::start().await;
    // Default: GET /items → 2 rows (same $.results envelope as search).
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [ { "id": 1 }, { "id": 2 } ]
        })))
        .mount(&server)
        .await;
    // When `q` is bound: POST /search with the rendered JSON body → 1 row.
    Mock::given(method("POST"))
        .and(path("/search"))
        .and(body_json(json!({ "query": "cats" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [ { "id": 7 } ]
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({ "base_url": server.uri() }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "items".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/items",
                "params": [ { "name": "q", "type": "varchar" } ],
                "requests": [
                    {
                        "when_filters": ["q"],
                        "endpoint": "/search",
                        "method": "POST",
                        "body": { "kind": "json", "template": "{\"query\": \"{q}\"}" }
                    }
                ],
                "response": {
                    "path": "$.results",
                    "schema": [
                        { "name": "id", "type": "bigint" },
                        { "name": "q",  "type": "varchar", "source": "param" }
                    ]
                }
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

    // No `q` → GET list (2 rows).
    assert_eq!(count(&ctx, "SELECT id FROM api.items").await, 2);
    // `q` bound → POST /search with the rendered body (1 row).
    assert_eq!(
        count(&ctx, "SELECT id FROM api.items WHERE q = 'cats'").await,
        1
    );
}
