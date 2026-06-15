//! Acceptance: request-side features — POST/GraphQL request bodies,
//! range/comparison filter pushdown, and header-aware rate limiting
//! (`extra_statuses`).

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
use wiremock::matchers::{body_json, header, method, path, query_param};
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

/// A POST table renders its `body.template`, substituting a bound param, and the
/// server only matches when the rendered JSON body is correct.
#[tokio::test]
async fn post_request_body_is_rendered() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .and(body_json(json!({ "q": "cats", "limit": 5 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [ { "id": 1 }, { "id": 2 } ]
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
            name: "search".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/search",
                "method": "POST",
                "params": [ { "name": "q", "type": "varchar", "required": true } ],
                "body": { "kind": "json", "template": "{\"q\": \"{q}\", \"limit\": 5}" },
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

    let batches = ctx
        .sql("SELECT id FROM api.search WHERE q = 'cats'")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute: rendered body should match the server");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 2);
}

/// `custom` auth `body` fields are merged into the table's JSON body: the server
/// only matches when both the rendered template and the auth fields are present.
#[tokio::test]
async fn custom_auth_body_merges_into_json_table_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .and(body_json(
            json!({ "q": "cats", "limit": 5, "api_key": "secret" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [ { "id": 1 } ]
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
        config: json!({
            "base_url": server.uri(),
            "auth": { "type": "custom", "body": [ { "name": "api_key", "value": "secret" } ] }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "search".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/search",
                "method": "POST",
                "params": [ { "name": "q", "type": "varchar", "required": true } ],
                "body": { "kind": "json", "template": "{\"q\": \"{q}\", \"limit\": 5}" },
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
    let batches = ctx
        .sql("SELECT id FROM api.search WHERE q = 'cats'")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute: merged body should match the server");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1);
}

/// With no table body, `custom` auth `body` fields are sent as the whole JSON body.
#[tokio::test]
async fn custom_auth_body_is_the_body_when_table_has_none() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ping"))
        .and(body_json(json!({ "api_key": "secret", "tenant": "acme" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 7 } ])))
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
            "auth": { "type": "custom", "body": [
                { "name": "api_key", "value": "secret" },
                { "name": "tenant",  "value": "acme" }
            ] }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "ping".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/ping",
                "method": "POST",
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
        .sql("SELECT id FROM api.ping")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute: auth body should be the request body");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1);
}

/// A non-JSON (form) table body cannot merge with `custom` auth `body` — the
/// query fails with a clear error rather than sending a malformed request.
#[tokio::test]
async fn custom_auth_body_rejects_non_json_table_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
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
            "auth": { "type": "custom", "body": [ { "name": "api_key", "value": "secret" } ] }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "form".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/form",
                "method": "POST",
                "params": [ { "name": "q", "type": "varchar", "required": true } ],
                "body": { "kind": "form", "template": "q={q}" },
                "response": { "path": "$", "schema": [
                    { "name": "id", "type": "bigint" },
                    { "name": "q",  "type": "varchar", "source": "param" }
                ] }
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
    let err = ctx
        .sql("SELECT id FROM api.form WHERE q = 'x'")
        .await
        .expect("plan")
        .collect()
        .await
        .expect_err("form body + custom auth body should error");
    assert!(
        err.to_string().contains("JSON table body"),
        "unexpected error: {err}"
    );
}

/// Comparison filters push down via a param's `accepts`/`emit` mapping: the
/// server only matches when both `since` and `until` query params are present.
#[tokio::test]
async fn range_filters_push_down() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .and(query_param("since", "2026-05-01"))
        .and(query_param("until", "2026-05-31"))
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
        config: json!({ "base_url": server.uri() }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "events".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/events",
                "params": [
                    {
                        "name": "created",
                        "type": "varchar",
                        "accepts": [">=", "<="],
                        "emit": { ">=": "since", "<=": "until" }
                    }
                ],
                "response": {
                    "path": "$",
                    "schema": [
                        { "name": "id",      "type": "bigint" },
                        { "name": "created", "type": "varchar", "source": "param" }
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

    let batches = ctx
        .sql(
            "SELECT id FROM api.events \
             WHERE created >= '2026-05-01' AND created <= '2026-05-31'",
        )
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute: range filters should emit since/until");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1);
}

/// Source-level `config.headers` are attached to every table's request: the
/// server only matches when both constant headers are present.
#[tokio::test]
async fn source_headers_apply_to_every_table() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("Accept", "application/vnd.github+json"))
        .and(header("X-GitHub-Api-Version", "2022-11-28"))
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
            "headers": {
                "Accept": "application/vnd.github+json",
                "X-GitHub-Api-Version": "2022-11-28"
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

    let batches = ctx
        .sql("SELECT id FROM api.items")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute: source headers should be sent on every request");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1);
}

/// A per-table `headers` entry overrides a source-level one on a key collision:
/// the server requires the table's `Accept`, not the source default.
#[tokio::test]
async fn table_headers_override_source_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("Accept", "application/json"))
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
            "headers": { "Accept": "application/vnd.github+json" }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "items".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/items",
                "headers": { "Accept": "application/json" },
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
        .expect("execute: per-table header should override the source default");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1);
}

/// `rate_limit.requests_per_second` throttles request issuance: with rps=1 the
/// second page must wait ~1s for the token bucket to refill. (Governor uses a
/// real monotonic clock, so this measures real elapsed time with a safe margin.)
#[tokio::test]
async fn requests_per_second_limits_throughput() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
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
            "rate_limit": { "requests_per_second": 1 }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "items".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/items",
                "pagination": { "type": "page", "param": "page", "start": 1 },
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

    let start = std::time::Instant::now();
    let batches = ctx
        .sql("SELECT id FROM api.items")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");
    let elapsed = start.elapsed();
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1, "page 1 (1 row) + page 2 (empty → stop)");
    assert!(
        elapsed >= std::time::Duration::from_millis(600),
        "rps=1 should make the 2nd request wait for the bucket, waited {elapsed:?}"
    );
}

/// A `derive: ago` param supplies a relative time-window default (`now - N`
/// epoch seconds) when the query doesn't filter on it — the value lands in the
/// request query string.
#[tokio::test]
async fn derive_ago_supplies_time_window_default() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
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
            name: "events".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/events",
                "params": [
                    { "name": "since", "type": "bigint", "derive": { "kind": "ago", "seconds": 3600 } }
                ],
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

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let _ = ctx
        .sql("SELECT id FROM api.events")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let since: i64 = requests[0]
        .url
        .query_pairs()
        .find(|(k, _)| k == "since")
        .map(|(_, v)| v.parse().expect("since is an integer"))
        .expect("since query param should be present");
    let expected = now - 3600;
    assert!(
        (since - expected).abs() <= 60,
        "since={since} should be ~now-3600 ({expected})"
    );
}

/// When the API reports its quota is exhausted via `remaining`/`reset` headers,
/// the next page waits until the reset time. Under a paused runtime the timer
/// auto-advances, so the elapsed (virtual) time reflects the throttle without a
/// real wall-clock wait.
#[tokio::test(start_paused = true)]
async fn rate_limit_headers_throttle_next_page() {
    let server = MockServer::start().await;
    let reset_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 50;
    // Page 1: quota exhausted (remaining=0), reset 50s out. Page 2: empty → stop.
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", reset_at.to_string().as_str())
                .set_body_json(json!([ { "id": 1 } ])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
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
            "rate_limit": {
                "remaining_header": "x-ratelimit-remaining",
                "reset_header": "x-ratelimit-reset"
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
                "pagination": { "type": "page", "param": "page", "start": 1 },
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

    let start = tokio::time::Instant::now();
    let batches = ctx
        .sql("SELECT id FROM api.items")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");
    let elapsed = start.elapsed();
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1, "only page 1 carried a row");
    assert!(
        elapsed >= std::time::Duration::from_secs(40),
        "the second page should wait for the reset window, waited {elapsed:?}"
    );
}

/// A status listed in `rate_limit.extra_statuses` (here GitHub-style `403`) is
/// retried like a 429 rather than surfaced as an error.
#[tokio::test]
async fn extra_status_403_is_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(403))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .with_priority(2)
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
            "retry": { "max_retries": 3, "base_backoff_ms": 1, "max_backoff_ms": 5 },
            "rate_limit": { "extra_statuses": [403] }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "items".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/items",
                "response": {
                    "path": "$",
                    "schema": [ { "name": "id", "type": "bigint" } ]
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

    let batches = ctx
        .sql("SELECT id FROM api.items")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute: 403 should be retried and then succeed");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1);
}
