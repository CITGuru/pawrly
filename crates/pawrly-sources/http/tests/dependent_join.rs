//! Acceptance: dependent (bind) joins — driving a required-`id` detail table
//! from the ids produced by another table, same-source and cross-source, bounded
//! by an enclosing `LIMIT`. Mirrors the HackerNews `top_stories → live_item`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use datafusion::arrow::array::{Array, Int64Array, StringArray};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider};
use datafusion::execution::config::SessionConfig;
use datafusion::execution::context::SessionContext;
use datafusion::execution::session_state::SessionStateBuilder;
use pawrly_core::{CachePolicy, SourceDef, SourceKind, TableDef};
use pawrly_sources_http::{DependentJoinRule, register_http_source};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A `SessionContext` with the dependent-join physical rule registered (as the
/// engine wires it), plus an empty `pawrly.default` catalog/schema.
async fn build_ctx() -> (SessionContext, Arc<MemoryCatalogProvider>) {
    let cfg = SessionConfig::new()
        .with_default_catalog_and_schema("pawrly", "default")
        .with_create_default_catalog_and_schema(false);
    let state = SessionStateBuilder::new()
        .with_config(cfg)
        .with_default_features()
        .with_physical_optimizer_rule(Arc::new(DependentJoinRule::new()))
        .build();
    let ctx = SessionContext::new_with_state(state);
    let catalog: Arc<MemoryCatalogProvider> = Arc::new(MemoryCatalogProvider::new());
    let default_schema: Arc<dyn datafusion::catalog::SchemaProvider> =
        Arc::new(datafusion::catalog::MemorySchemaProvider::new());
    let _ = catalog.register_schema("default", default_schema).unwrap();
    ctx.register_catalog("pawrly", catalog.clone());
    (ctx, catalog)
}

/// One `http` source `api` exposing a ranked-id `top` driver table and a
/// required-`id` `item` detail table, both against `base_url`.
fn driver_detail_source(base_url: String, detail_endpoint: &str) -> SourceDef {
    SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({ "base_url": base_url }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![
            TableDef {
                name: "top".into(),
                description: None,
                wiki: None,
                config: json!({
                    "endpoint": "/top",
                    "response": { "path": "$", "schema": [ { "name": "id", "type": "bigint" } ] }
                }),
                cache: None,
                safety: None,
            },
            TableDef {
                name: "item".into(),
                description: None,
                wiki: None,
                config: json!({
                    "endpoint": detail_endpoint,
                    "params": [ { "name": "id", "type": "bigint", "required": true } ],
                    "response": { "path": "$", "schema": [
                        { "name": "id",    "type": "bigint" },
                        { "name": "title", "type": "varchar" },
                        { "name": "score", "type": "bigint" }
                    ] }
                }),
                cache: None,
                safety: None,
            },
        ],
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

fn row_count(batches: &[RecordBatch]) -> usize {
    batches.iter().map(RecordBatch::num_rows).sum()
}

/// Every value of a string column across all batches, sorted.
fn str_values(batches: &[RecordBatch], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for b in batches {
        let col = b
            .column_by_name(name)
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for i in 0..col.len() {
            out.push(col.value(i).to_string());
        }
    }
    out.sort();
    out
}

/// Mount `/top` returning the given ids and `/item/<id>` (or the detail path) for
/// each, titled `t<id>` with score `id*10`.
async fn mount_top_and_items(server: &MockServer, ids: &[i64]) {
    let top: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
    Mock::given(method("GET"))
        .and(path("/top"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Value::Array(top)))
        .mount(server)
        .await;
    for &id in ids {
        Mock::given(method("GET"))
            .and(path(format!("/item/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": id, "title": format!("t{id}"), "score": id * 10
            })))
            .mount(server)
            .await;
    }
}

/// Same-source dependent join: a ranked-id list drives a required-`id` detail
/// table, the key bound from the driver at runtime.
#[tokio::test]
async fn dependent_join_same_source() {
    let server = MockServer::start().await;
    mount_top_and_items(&server, &[1, 2, 3]).await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &driver_detail_source(server.uri(), "/item/{id}"),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let batches = query(
        &ctx,
        "SELECT i.title, i.score FROM api.top t JOIN api.item i ON i.id = t.id",
    )
    .await;
    assert_eq!(row_count(&batches), 3, "all three ids enriched");
    assert_eq!(str_values(&batches, "title"), vec!["t1", "t2", "t3"]);
}

/// The detail fan-out is bounded by an enclosing `LIMIT`: with three ranked ids
/// but `LIMIT 2`, only two detail lookups fire (not all three).
#[tokio::test]
async fn dependent_join_bounded_by_limit() {
    let server = MockServer::start().await;
    mount_top_and_items(&server, &[1, 2, 3]).await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &driver_detail_source(server.uri(), "/item/{id}"),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let batches = query(
        &ctx,
        "SELECT i.title FROM api.top t JOIN api.item i ON i.id = t.id LIMIT 2",
    )
    .await;
    assert_eq!(row_count(&batches), 2, "LIMIT 2 keeps two rows");

    let detail_requests = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().starts_with("/item/"))
        .count();
    assert_eq!(
        detail_requests, 2,
        "fan-out must be bounded by the limit, not issue all three lookups"
    );
}

/// `ORDER BY … LIMIT` must NOT cap the key fan-out: a TopK needs every candidate
/// to find the real top rows. With three ids but `ORDER BY score DESC LIMIT 2`,
/// all three are fetched and the correct top two are returned.
#[tokio::test]
async fn order_by_limit_fetches_all_candidates() {
    let server = MockServer::start().await;
    mount_top_and_items(&server, &[1, 2, 3]).await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &driver_detail_source(server.uri(), "/item/{id}"),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let batches = query(
        &ctx,
        "SELECT i.title FROM api.top t JOIN api.item i ON i.id = t.id \
         ORDER BY i.score DESC LIMIT 2",
    )
    .await;
    assert_eq!(row_count(&batches), 2, "LIMIT 2 keeps two rows");
    // Highest scores are id 3 then id 2 (score = id*10).
    assert_eq!(str_values(&batches, "title"), vec!["t2", "t3"]);

    let detail_requests = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().starts_with("/item/"))
        .count();
    assert_eq!(
        detail_requests, 3,
        "a sort+limit must fetch every candidate, not cap before sorting"
    );
}

/// Cross-host dependent join: the detail table lives on a different server than
/// the driver (absolute endpoint), proving the bind works across upstreams — the
/// HN Algolia-`items` ↔ Firebase-`top_stories` shape.
#[tokio::test]
async fn dependent_join_cross_host() {
    let driver = MockServer::start().await;
    let detail = MockServer::start().await;
    // Driver server hosts /top; detail server hosts /item/<id>.
    Mock::given(method("GET"))
        .and(path("/top"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 }, { "id": 2 } ])))
        .mount(&driver)
        .await;
    for id in 1..=2 {
        Mock::given(method("GET"))
            .and(path(format!("/item/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": id, "title": format!("t{id}"), "score": id * 10
            })))
            .mount(&detail)
            .await;
    }

    let (ctx, catalog) = build_ctx().await;
    // Detail endpoint is an absolute URL on the *other* host.
    let detail_endpoint = format!("{}/item/{{id}}", detail.uri());
    register_http_source(
        &driver_detail_source(driver.uri(), &detail_endpoint),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let batches = query(
        &ctx,
        "SELECT i.title FROM api.top t JOIN api.item i ON i.id = t.id",
    )
    .await;
    assert_eq!(row_count(&batches), 2);
    assert_eq!(str_values(&batches, "title"), vec!["t1", "t2"]);
}

/// Regression guard: a bare scan of the required-`id` detail table (no join, no
/// id) still fails with the safety code — deferral must not silently succeed.
#[tokio::test]
async fn bare_required_scan_still_errors() {
    let server = MockServer::start().await;
    mount_top_and_items(&server, &[1]).await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &driver_detail_source(server.uri(), "/item/{id}"),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let err = ctx
        .sql("SELECT title FROM api.item")
        .await
        .expect("plan")
        .collect()
        .await
        .expect_err("missing required id should error");
    assert!(
        err.to_string().contains("PAWRLY_SAFETY_REQUIRED_FILTER"),
        "bare scan should still raise the safety code: {err}"
    );
}

/// A literal `id` still resolves the detail table directly, unaffected by the
/// dependent-join machinery (single-id regression guard, with the rule active).
#[tokio::test]
async fn literal_id_detail_still_works() {
    let server = MockServer::start().await;
    mount_top_and_items(&server, &[7]).await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &driver_detail_source(server.uri(), "/item/{id}"),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let batches = query(&ctx, "SELECT title, score FROM api.item WHERE id = 7").await;
    assert_eq!(row_count(&batches), 1);
    let score = batches[0]
        .column_by_name("score")
        .unwrap()
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap();
    assert_eq!(score.value(0), 70);
}
