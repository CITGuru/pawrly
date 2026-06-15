//! Acceptance: computed response columns (`expr`), `explode` IN-list pushdown,
//! `derive`d param defaults, and the `raw_table` escape hatch.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use datafusion::arrow::array::{Int32Array, StringArray};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider};
use datafusion::execution::config::SessionConfig;
use datafusion::execution::context::SessionContext;
use pawrly_core::{CachePolicy, SourceDef, SourceKind, TableDef};
use pawrly_sources_http::register_http_source;
use serde_json::{Value, json};
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

/// A single-table `http` source named `api.data` with the given table `config`.
fn typed_source(base_url: String, table_config: Value) -> SourceDef {
    SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({ "base_url": base_url }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "data".into(),
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

async fn query(ctx: &SessionContext, sql: &str) -> Vec<RecordBatch> {
    ctx.sql(sql)
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute")
}

fn str_col<'a>(b: &'a RecordBatch, name: &str) -> &'a StringArray {
    b.column_by_name(name)
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap()
}

/// An `explode` param pushes `IN (a, b)` down as repeated query pairs
/// (`?status=a&status=b`) — the server only matches when both are present.
#[tokio::test]
async fn explode_in_list_repeats_query_pairs() {
    let server = MockServer::start().await;
    // Match the path only; assert the exploded query string via the recorder.
    Mock::given(method("GET"))
        .and(path("/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 }, { "id": 2 } ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &typed_source(
            server.uri(),
            json!({
                "endpoint": "/issues",
                "params": [ { "name": "state", "type": "varchar", "explode": true } ],
                "response": { "path": "$", "schema": [
                    { "name": "id",    "type": "bigint" },
                    { "name": "state", "type": "varchar", "source": "param" }
                ] }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let _ = query(
        &ctx,
        "SELECT id FROM api.data WHERE state IN ('open', 'closed', 'merged')",
    )
    .await;

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "exactly one request should be issued");
    let q = requests[0].url.query().unwrap_or_default();
    assert!(
        q.contains("state=open") && q.contains("state=closed") && q.contains("state=merged"),
        "IN(...) should explode into repeated query pairs, got query: {q}"
    );
}

/// `NOT IN (...)` must not explode (it lowers to a conjunction of inequalities,
/// not a disjunction of equalities) — no `state` query params are emitted.
#[tokio::test]
async fn not_in_does_not_explode() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &typed_source(
            server.uri(),
            json!({
                "endpoint": "/issues",
                "params": [ { "name": "state", "type": "varchar", "explode": true } ],
                "response": { "path": "$", "schema": [
                    { "name": "id",    "type": "bigint" },
                    { "name": "state", "type": "varchar", "source": "param" }
                ] }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let _ = query(
        &ctx,
        "SELECT id FROM api.data WHERE state NOT IN ('open', 'closed')",
    )
    .await;

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0]
            .url
            .query()
            .unwrap_or_default()
            .contains("state="),
        "NOT IN must not be pushed down as exploded query pairs"
    );
}

/// Computed `expr` columns evaluate end-to-end through a SQL scan: coalesce,
/// map_join, to_timestamp, from_base64, lookup, and from_filter.
#[tokio::test]
async fn computed_expr_columns_evaluate() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 7,
                "attributes": { "title": "Primary Title" },
                "labels": [ { "name": "bug" }, { "name": "p1" } ],
                "created_epoch": 1_700_000_000,
                "content_b64": "aGVsbG8=",
                "headers": [
                    { "name": "From", "value": "a@x" },
                    { "name": "To",   "value": "b@y" }
                ]
            }
        ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &typed_source(
            server.uri(),
            json!({
                "endpoint": "/rows",
                "params": [ { "name": "q", "type": "varchar" } ],
                "response": {
                    "path": "$",
                    "schema": [
                        {
                            "name": "title", "type": "varchar",
                            "expr": { "kind": "coalesce", "exprs": [
                                { "kind": "path", "path": ["attributes", "title"] },
                                { "kind": "path", "path": ["title"] }
                            ] }
                        },
                        {
                            "name": "labels_csv", "type": "varchar",
                            "expr": { "kind": "map_join", "path": ["labels"], "item_path": ["name"] }
                        },
                        {
                            "name": "created_at", "type": "varchar",
                            "expr": { "kind": "to_timestamp", "unit": "seconds",
                                      "expr": { "kind": "path", "path": ["created_epoch"] } }
                        },
                        {
                            "name": "content", "type": "varchar",
                            "expr": { "kind": "from_base64",
                                      "expr": { "kind": "path", "path": ["content_b64"] } }
                        },
                        {
                            "name": "from_addr", "type": "varchar",
                            "expr": { "kind": "lookup", "path": ["headers"], "key": "From",
                                      "key_field": "name", "value_field": "value" }
                        },
                        {
                            "name": "echoed", "type": "varchar",
                            "expr": { "kind": "from_filter", "filter": "q" }
                        },
                        { "name": "q", "type": "varchar", "source": "param" }
                    ]
                }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let batches = query(
        &ctx,
        "SELECT title, labels_csv, created_at, content, from_addr, echoed \
         FROM api.data WHERE q = 'ping'",
    )
    .await;
    let b = &batches[0];
    assert_eq!(b.num_rows(), 1);
    assert_eq!(str_col(b, "title").value(0), "Primary Title");
    assert_eq!(str_col(b, "labels_csv").value(0), "bug,p1");
    assert_eq!(
        str_col(b, "created_at").value(0),
        "2023-11-14T22:13:20+00:00"
    );
    assert_eq!(str_col(b, "content").value(0), "hello");
    assert_eq!(str_col(b, "from_addr").value(0), "a@x");
    assert_eq!(str_col(b, "echoed").value(0), "ping");
}

/// A `derive: split` param computes a path placeholder from another bound param:
/// `repo = 'octocat/hello'` derives `owner = 'octocat'`, which fills `/repos/{owner}`.
#[tokio::test]
async fn derive_split_fills_path_param() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/octocat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &typed_source(
            server.uri(),
            json!({
                "endpoint": "/repos/{owner}",
                "params": [
                    { "name": "repo",  "type": "varchar" },
                    {
                        "name": "owner", "type": "varchar",
                        "derive": { "kind": "split", "from": "repo", "separator": "/", "part": 0 }
                    }
                ],
                "response": { "path": "$", "schema": [
                    { "name": "id",   "type": "bigint" },
                    { "name": "repo", "type": "varchar", "source": "param" }
                ] }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let rows: usize = query(&ctx, "SELECT id FROM api.data WHERE repo = 'octocat/hello'")
        .await
        .iter()
        .map(|b| b.num_rows())
        .sum();
    assert_eq!(rows, 1, "owner derived from repo should fill the path");
}

/// The `raw_table` escape hatch exposes the source as a single table named after
/// the source: a `request_path` filter drives one request and the response
/// status/body land in virtual columns.
#[tokio::test]
async fn raw_table_fetches_by_request_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/anything"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
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
        tables: Vec::new(),
        raw_table: true,
        raw_table_safety: None,
    };
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let batches = query(
        &ctx,
        "SELECT response_status, response_body FROM api WHERE request_path = '/anything'",
    )
    .await;
    let b = &batches[0];
    assert_eq!(b.num_rows(), 1);
    let status = b
        .column_by_name("response_status")
        .unwrap()
        .as_any()
        .downcast_ref::<Int32Array>()
        .unwrap()
        .value(0);
    assert_eq!(status, 200);
    assert!(
        str_col(b, "response_body").value(0).contains("\"ok\":true"),
        "raw body should carry the JSON payload"
    );
}

/// The raw table refuses a scan with no `request_path` filter rather than
/// fanning out an unbounded request.
#[tokio::test]
async fn raw_table_requires_request_path_filter() {
    let server = MockServer::start().await;
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
        tables: Vec::new(),
        raw_table: true,
        raw_table_safety: None,
    };
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let err = ctx
        .sql("SELECT response_status FROM api")
        .await
        .expect("plan")
        .collect()
        .await
        .expect_err("missing request_path filter should error");
    assert!(
        err.to_string().contains("request_path"),
        "error should name the required filter: {err}"
    );
}
