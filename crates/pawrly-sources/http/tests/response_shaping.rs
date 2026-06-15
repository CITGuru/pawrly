//! Acceptance: typed-table response shaping — column types and string coercion,
//! the whole-row (`$`) source, positional/nested response paths, single-object
//! payloads, `dict_entries`/`series_points` reshapes, computed `expr` timestamps,
//! LIMIT-driven early pagination stop, and 404/`error.status` handling.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use datafusion::arrow::array::{Array, Float64Array, Int64Array, StringArray};
use datafusion::arrow::datatypes::{DataType, TimeUnit};
use datafusion::arrow::record_batch::RecordBatch;
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

/// Build a single-table `http` source named `api.data`, with caller-supplied
/// source `config` (so tests can add `retry`, etc.) and table `config`.
fn def(source_config: Value, table_config: Value) -> SourceDef {
    SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: source_config,
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

/// A response whose rows are nested at a positional array index — World Bank's
/// `[pagination_meta, [rows...]]` shape — is reachable via `path: $[1]`.
#[tokio::test]
async fn positional_array_index_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pop"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "page": 1, "pages": 1, "total": 2 },
            [
                { "country": { "value": "United States" }, "date": "2024", "val": 340110988 },
                { "country": { "value": "Nigeria" },       "date": "2024", "val": 227882945 }
            ]
        ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            json!({ "base_url": server.uri() }),
            json!({
                "endpoint": "/pop",
                "response": {
                    "path": "$[1]",
                    "schema": [
                        { "name": "country", "type": "varchar", "source": "$.country.value" },
                        { "name": "date",    "type": "varchar" },
                        { "name": "val",     "type": "bigint" }
                    ]
                }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let batches = query(&ctx, "SELECT country, val FROM api.data ORDER BY val DESC").await;
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 2, "both rows under $[1] should load");
    assert_eq!(str_col(&batches[0], "country").value(0), "United States");
}

/// A key followed by an index (`$.response[1]`) chains object and array access.
#[tokio::test]
async fn nested_key_then_index_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wrapped"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "response": [
                { "meta": true },
                [ { "id": 1 }, { "id": 2 }, { "id": 3 } ]
            ]
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            json!({ "base_url": server.uri() }),
            json!({
                "endpoint": "/wrapped",
                "response": {
                    "path": "$.response[1]",
                    "schema": [ { "name": "id", "type": "bigint" } ]
                }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let rows: usize = query(&ctx, "SELECT id FROM api.data")
        .await
        .iter()
        .map(|b| b.num_rows())
        .sum();
    assert_eq!(rows, 3);
}

/// A single JSON object at the response path (not an array) becomes exactly one
/// row — e.g. an FX-rates endpoint returning one record.
#[tokio::test]
async fn single_object_becomes_one_row() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "base": "USD",
            "date": "2026-06-15",
            "rates": { "EUR": 0.86, "GBP": 0.74 }
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            json!({ "base_url": server.uri() }),
            json!({
                "endpoint": "/latest",
                "response": {
                    "path": "$",
                    "schema": [
                        { "name": "base", "type": "varchar" },
                        { "name": "eur",  "type": "double", "source": "$.rates.EUR" }
                    ]
                }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let batches = query(&ctx, "SELECT base, eur FROM api.data").await;
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1, "a lone object should yield a single row");
    assert_eq!(str_col(&batches[0], "base").value(0), "USD");
    let eur = batches[0]
        .column_by_name("eur")
        .unwrap()
        .as_any()
        .downcast_ref::<Float64Array>()
        .unwrap()
        .value(0);
    assert!((eur - 0.86).abs() < 1e-9);
}

/// `reshape: dict_entries` over an object of objects emits one row per entry,
/// exposing the map key as `_key`.
#[tokio::test]
async fn dict_entries_object_values() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/colors"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "primary":   { "hex": "#fff" },
            "secondary": { "hex": "#eee" }
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            json!({ "base_url": server.uri() }),
            json!({
                "endpoint": "/colors",
                "response": {
                    "path": "$",
                    "reshape": { "kind": "dict_entries" },
                    "schema": [
                        { "name": "_key", "type": "varchar" },
                        { "name": "hex",  "type": "varchar" }
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
        "SELECT _key, hex FROM api.data WHERE _key = 'primary'",
    )
    .await;
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1);
    assert_eq!(str_col(&batches[0], "hex").value(0), "#fff");
}

/// `reshape: dict_entries` over an object of *scalars* emits `{_key, _value}`.
#[tokio::test]
async fn dict_entries_scalar_values() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/counts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "open": 3, "closed": 7
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            json!({ "base_url": server.uri() }),
            json!({
                "endpoint": "/counts",
                "response": {
                    "path": "$",
                    "reshape": { "kind": "dict_entries" },
                    "schema": [
                        { "name": "_key",   "type": "varchar" },
                        { "name": "_value", "type": "bigint" }
                    ]
                }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let batches = query(&ctx, "SELECT _value FROM api.data WHERE _key = 'closed'").await;
    let v = batches[0]
        .column_by_name("_value")
        .unwrap()
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap()
        .value(0);
    assert_eq!(v, 7);
}

/// `reshape: series_points` flattens a `{series:[{…, points:[[t,v]]}]}` payload
/// into one row per point, carrying series fields plus `timestamp`/`value`.
#[tokio::test]
async fn series_points_reshape() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "series": [
                {
                    "metric": "cpu",
                    "scope": "host:a",
                    "pointlist": [[1000, 0.5], [2000, 0.7], [3000, 0.9]]
                }
            ]
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            json!({ "base_url": server.uri() }),
            json!({
                "endpoint": "/metrics",
                "response": {
                    "path": "$",
                    "reshape": {
                        "kind": "series_points",
                        "series": "series",
                        "points": "pointlist",
                        "timestamp": "ts",
                        "value": "val"
                    },
                    "schema": [
                        { "name": "metric", "type": "varchar" },
                        { "name": "ts",     "type": "bigint" },
                        { "name": "val",    "type": "double" }
                    ]
                }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let batches = query(&ctx, "SELECT metric, ts, val FROM api.data ORDER BY ts").await;
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 3, "three points should explode into three rows");
    assert_eq!(str_col(&batches[0], "metric").value(0), "cpu");
}

/// A `timestamp` column parses the non-RFC3339 fallback formats: a
/// space-separated datetime, fractional seconds, and a bare date (midnight UTC).
#[tokio::test]
async fn timestamp_column_parses_fallback_formats() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "t": "2026-05-31 12:00:00" },
            { "id": 2, "t": "2026-05-31T12:00:00.500" },
            { "id": 3, "t": "2026-05-31" }
        ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            json!({ "base_url": server.uri() }),
            json!({
                "endpoint": "/events",
                "response": {
                    "path": "$",
                    "schema": [
                        { "name": "id", "type": "bigint" },
                        { "name": "t",  "type": "timestamp" }
                    ]
                }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let count = |sql: &'static str| {
        let ctx = ctx.clone();
        async move {
            let b = query(&ctx, sql).await;
            b[0].column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .value(0)
        }
    };
    assert_eq!(
        count("SELECT count(*) FROM api.data WHERE t IS NOT NULL").await,
        3
    );
    assert_eq!(
        count("SELECT count(*) FROM api.data WHERE t = TIMESTAMP '2026-05-31 12:00:00'").await,
        1
    );
    assert_eq!(
        count("SELECT count(*) FROM api.data WHERE t = TIMESTAMP '2026-05-31 12:00:00.500'").await,
        1
    );
    assert_eq!(
        count("SELECT count(*) FROM api.data WHERE t = TIMESTAMP '2026-05-31 00:00:00'").await,
        1
    );
}

/// Numbers and booleans that arrive as JSON strings (`"12.50"`, `"true"`) coerce
/// into `double`/`boolean` columns instead of becoming NULL.
#[tokio::test]
async fn double_and_bool_columns_coerce_from_strings() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "price": "12.50", "active": "true" },
            { "price": 3.5,      "active": false }
        ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            json!({ "base_url": server.uri() }),
            json!({
                "endpoint": "/rows",
                "response": {
                    "path": "$",
                    "schema": [
                        { "name": "price",  "type": "double" },
                        { "name": "active", "type": "boolean" }
                    ]
                }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let batches = query(&ctx, "SELECT price, active FROM api.data").await;
    let price = batches[0]
        .column_by_name("price")
        .unwrap()
        .as_any()
        .downcast_ref::<Float64Array>()
        .unwrap();
    assert!(
        !price.is_null(0),
        "stringified decimal must not become NULL"
    );
    assert!((price.value(0) - 12.50).abs() < 1e-9);
    assert!((price.value(1) - 3.5).abs() < 1e-9);

    let active = batches[0]
        .column_by_name("active")
        .unwrap()
        .as_any()
        .downcast_ref::<datafusion::arrow::array::BooleanArray>()
        .unwrap();
    assert!(!active.is_null(0), "stringified bool must not become NULL");
    assert!(active.value(0));
    assert!(!active.value(1));
}

/// A `series_points` reshape over multiple series with float epoch-millis
/// timestamps: the timestamps must land in a `bigint` column, not become NULL.
#[tokio::test]
async fn series_points_multi_series_float_millis() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "series": [
                {
                    "metric": "system.cpu",
                    "scope": "host:a",
                    "pointlist": [[1_700_000_000_000.0, 0.5], [1_700_000_060_000.0, 0.7]]
                },
                {
                    "metric": "system.cpu",
                    "scope": "host:b",
                    "pointlist": [[1_700_000_000_000.0, 0.9]]
                }
            ]
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            json!({ "base_url": server.uri() }),
            json!({
                "endpoint": "/query",
                "response": {
                    "path": "$",
                    "reshape": {
                        "kind": "series_points",
                        "series": "series",
                        "points": "pointlist",
                        "timestamp": "ts",
                        "value": "val"
                    },
                    "schema": [
                        { "name": "metric", "type": "varchar" },
                        { "name": "scope",  "type": "varchar" },
                        { "name": "ts",     "type": "bigint" },
                        { "name": "val",    "type": "double" }
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
        "SELECT metric, scope, ts, val FROM api.data ORDER BY scope, ts",
    )
    .await;
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total, 3, "two series should flatten to three point rows");

    let ts = batches[0]
        .column_by_name("ts")
        .unwrap()
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap();
    assert!(
        !ts.is_null(0),
        "float epoch-millis must not become NULL in bigint"
    );
    assert_eq!(ts.value(0), 1_700_000_000_000);
    assert_eq!(str_col(&batches[0], "scope").value(0), "host:a");
}

/// `error.status` matching a `5xx` expression turns a server error into a clear
/// scan failure (rather than an opaque parse error).
#[tokio::test]
async fn error_status_5xx_matcher_fails_scan() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/flaky"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({})))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            // Disable retry so the test doesn't pay backoff for a deterministic 503.
            json!({ "base_url": server.uri(), "retry": { "max_retries": 0 } }),
            json!({
                "endpoint": "/flaky",
                "response": {
                    "path": "$",
                    "error": { "status": ["5xx"] },
                    "schema": [ { "name": "id", "type": "bigint" } ]
                }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let err = ctx
        .sql("SELECT id FROM api.data")
        .await
        .expect("plan")
        .collect()
        .await
        .expect_err("a 5xx should fail the scan when declared in error.status");
    assert!(
        err.to_string().contains("503"),
        "scan error should mention the status: {err}"
    );
}

/// `error.status` accepts an exact code and an inequality expression; both turn
/// a matching response into a scan error.
#[tokio::test]
async fn error_status_exact_and_inequality_matchers() {
    for (status, matcher) in [(418u16, json!(418)), (404u16, json!(">=400"))] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({})))
            .mount(&server)
            .await;

        let (ctx, catalog) = build_ctx().await;
        register_http_source(
            &def(
                json!({ "base_url": server.uri(), "retry": { "max_retries": 0 } }),
                json!({
                    "endpoint": "/x",
                    "response": {
                        "path": "$",
                        "error": { "status": [matcher] },
                        "schema": [ { "name": "id", "type": "bigint" } ]
                    }
                }),
            ),
            &ctx,
            catalog.as_ref(),
        )
        .await
        .expect("register");

        let err = ctx
            .sql("SELECT id FROM api.data")
            .await
            .expect("plan")
            .collect()
            .await
            .expect_err("declared error status should fail the scan");
        assert!(
            err.to_string().contains(&status.to_string()),
            "error should mention status {status}: {err}"
        );
    }
}

/// A response path digs into a nested object map, then `dict_entries` turns it
/// into rows — the Frankfurter `{ rates: { EUR: .., GBP: .. } }` shape.
#[tokio::test]
async fn nested_path_then_dict_entries_reshape() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "base": "USD",
            "rates": { "EUR": 0.86, "GBP": 0.74, "JPY": 160.2 }
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            json!({ "base_url": server.uri() }),
            json!({
                "endpoint": "/latest",
                "response": {
                    "path": "$.rates",
                    "reshape": { "kind": "dict_entries" },
                    "schema": [
                        { "name": "currency", "type": "varchar", "source": "$._key" },
                        { "name": "rate",     "type": "double",  "source": "$._value" }
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
        "SELECT currency, rate FROM api.data WHERE currency = 'JPY'",
    )
    .await;
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1);
    assert_eq!(str_col(&batches[0], "currency").value(0), "JPY");
}

/// A computed `to_timestamp` feeding a real `timestamp` column parses through to
/// an Arrow timestamp (the RFC3339 `+00:00` form must round-trip).
#[tokio::test]
async fn computed_to_timestamp_into_timestamp_column() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "epoch": 1_700_000_000 }
        ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    register_http_source(
        &def(
            json!({ "base_url": server.uri() }),
            json!({
                "endpoint": "/events",
                "response": {
                    "path": "$",
                    "schema": [
                        {
                            "name": "at", "type": "timestamp",
                            "expr": { "kind": "to_timestamp", "unit": "seconds",
                                      "expr": { "kind": "path", "path": ["epoch"] } }
                        }
                    ]
                }
            }),
        ),
        &ctx,
        catalog.as_ref(),
    )
    .await
    .expect("register");

    let n: i64 = {
        let batches = query(
            &ctx,
            "SELECT count(*) AS c FROM api.data WHERE at = TIMESTAMP '2023-11-14T22:13:20'",
        )
        .await;
        batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0)
    };
    assert_eq!(n, 1, "to_timestamp should populate the timestamp column");
}
