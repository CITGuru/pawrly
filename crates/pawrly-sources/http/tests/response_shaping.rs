//! Acceptance: typed-table response shaping — temporal/json column types, the
//! whole-row (`$`) column source, LIMIT-driven early pagination stop, and
//! 404/error handling.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use datafusion::arrow::array::{Int64Array, StringArray};
use datafusion::arrow::datatypes::{DataType, TimeUnit};
use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider};
use datafusion::execution::config::SessionConfig;
use datafusion::execution::context::SessionContext;
use pawrly_core::{CachePolicy, SourceDef, SourceKind, TableDef};
use pawrly_sources_http::register_http_source;
use serde_json::{Value, json};
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

fn source(name: &str, base_url: String, table: TableDef) -> SourceDef {
    SourceDef {
        name: name.into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({ "base_url": base_url }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![table],
        raw_table: false,
        raw_table_safety: None,
    }
}

/// `timestamp`, `date`, and `json` types map to the right Arrow types, parse
/// from strings, and the `$` source captures the whole row element as JSON.
#[tokio::test]
async fn temporal_and_json_types() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 1,
                "created_at": "2026-05-31T12:00:00Z",
                "due": "2026-05-31",
                "meta": { "k": "v", "n": 3 }
            }
        ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let table = TableDef {
        name: "events".into(),
        description: None,
        wiki: None,
        config: json!({
            "endpoint": "/events",
            "response": {
                "path": "$",
                "schema": [
                    { "name": "id",         "type": "bigint" },
                    { "name": "created_at", "type": "timestamp" },
                    { "name": "due",        "type": "date" },
                    { "name": "meta",       "type": "json" },
                    { "name": "raw",        "type": "json", "source": "$" }
                ]
            }
        }),
        cache: None,
        safety: None,
    };
    register_http_source(&source("api", server.uri(), table), &ctx, catalog.as_ref())
        .await
        .expect("register");

    let batches = ctx
        .sql("SELECT id, created_at, due, meta, raw FROM api.events")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");
    let schema = batches[0].schema();
    assert_eq!(
        schema.field_with_name("created_at").unwrap().data_type(),
        &DataType::Timestamp(TimeUnit::Microsecond, None)
    );
    assert_eq!(
        schema.field_with_name("due").unwrap().data_type(),
        &DataType::Date32
    );

    // Temporal values parse correctly (verified via SQL literal comparison).
    let n = ctx
        .sql(
            "SELECT count(*) AS c FROM api.events \
             WHERE created_at = TIMESTAMP '2026-05-31T12:00:00' AND due = DATE '2026-05-31'",
        )
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");
    let c = n[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap()
        .value(0);
    assert_eq!(c, 1, "timestamp/date should parse and match the literals");

    // `meta` is a JSON column carrying the nested object as text; `raw` ($) is
    // the entire row element.
    let meta = batches[0]
        .column_by_name("meta")
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap()
        .value(0);
    let meta_json: Value = serde_json::from_str(meta).unwrap();
    assert_eq!(meta_json, json!({ "k": "v", "n": 3 }));

    let raw = batches[0]
        .column_by_name("raw")
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap()
        .value(0);
    let raw_json: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(raw_json["id"], json!(1));
    assert_eq!(raw_json["meta"]["k"], json!("v"));
}

/// A LIMIT stops pagination as soon as enough rows are collected — the next
/// page (here, unmounted) is never requested.
#[tokio::test]
async fn limit_stops_pagination_early() {
    let server = MockServer::start().await;
    // Only page 1 exists; page 2 would 404 and fail the scan if we walked it.
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1 }, { "id": 2 }
        ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let table = TableDef {
        name: "items".into(),
        description: None,
        wiki: None,
        config: json!({
            "endpoint": "/items",
            "pagination": { "type": "page", "param": "page", "start": 1 },
            "response": {
                "path": "$",
                "schema": [ { "name": "id", "type": "bigint" } ]
            }
        }),
        cache: None,
        safety: None,
    };
    register_http_source(&source("api", server.uri(), table), &ctx, catalog.as_ref())
        .await
        .expect("register");

    let batches = ctx
        .sql("SELECT id FROM api.items LIMIT 2")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute: LIMIT should stop before requesting page 2");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 2);
}

/// `allow_404_empty` turns a 404 into an empty result instead of an error.
#[tokio::test]
async fn allow_404_empty_yields_no_rows() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let table = TableDef {
        name: "missing".into(),
        description: None,
        wiki: None,
        config: json!({
            "endpoint": "/missing",
            "response": {
                "path": "$",
                "allow_404_empty": true,
                "schema": [ { "name": "id", "type": "bigint" } ]
            }
        }),
        cache: None,
        safety: None,
    };
    register_http_source(&source("api", server.uri(), table), &ctx, catalog.as_ref())
        .await
        .expect("register");

    let batches = ctx
        .sql("SELECT id FROM api.missing")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 0);
}

/// A 200-with-error body, matched by `error.path`, fails the scan with the
/// extracted message.
#[tokio::test]
async fn error_envelope_fails_scan() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/things"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": { "message": "rate limited, slow down" }
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let table = TableDef {
        name: "things".into(),
        description: None,
        wiki: None,
        config: json!({
            "endpoint": "/things",
            "response": {
                "path": "$.data",
                "error": { "path": "$.error.message" },
                "schema": [ { "name": "id", "type": "bigint" } ]
            }
        }),
        cache: None,
        safety: None,
    };
    register_http_source(&source("api", server.uri(), table), &ctx, catalog.as_ref())
        .await
        .expect("register");

    let err = ctx
        .sql("SELECT id FROM api.things")
        .await
        .expect("plan")
        .collect()
        .await
        .expect_err("error envelope should fail the scan");
    assert!(
        err.to_string().contains("rate limited"),
        "scan error should carry the API message: {err}"
    );
}
