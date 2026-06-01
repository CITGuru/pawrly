//! Acceptance: bundled github source against a wiremock GitHub.

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
use serde_json::json;
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

#[tokio::test]
async fn typed_github_pulls_filter_pushes_to_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/pawrly/pawrly/pulls"))
        .and(query_param("state", "open"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "number": 42, "title": "Fix flaky test", "state": "open", "html_url": "https://github.com/pawrly/pawrly/pull/42" },
            { "number": 41, "title": "Doc tweak",     "state": "open", "html_url": "https://github.com/pawrly/pawrly/pull/41" }
        ])))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;

    let def = SourceDef {
        name: "gh".into(),
        kind: SourceKind::Github,
        description: None,
        config: json!({
            "base_url": server.uri(),
            "token": "test-token"
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: Vec::new(),
        raw_table: false,
        raw_table_safety: None,
    };

    let report = register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register github");
    assert!(report.tables.iter().any(|t| t.name == "pulls"));

    let df = ctx
        .sql("SELECT number, title FROM gh.pulls WHERE owner = 'pawrly' AND repo = 'pawrly' AND state = 'open'")
        .await
        .expect("plan");
    let batches = df.collect().await.expect("execute");
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 2);
}

#[tokio::test]
async fn typed_github_pulls_missing_required_filter_errors() {
    let server = MockServer::start().await;
    let (ctx, catalog) = build_ctx().await;

    let def = SourceDef {
        name: "gh".into(),
        kind: SourceKind::Github,
        description: None,
        config: json!({
            "base_url": server.uri(),
            "token": "test-token"
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: Vec::new(),
        raw_table: false,
        raw_table_safety: None,
    };

    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register github");

    let df = ctx.sql("SELECT * FROM gh.pulls").await.expect("plan");
    let err = df
        .collect()
        .await
        .expect_err("missing required filter should error");
    let s = err.to_string();
    assert!(
        s.contains("PAWRLY_SAFETY_REQUIRED_FILTER"),
        "unexpected error: {s}"
    );
}

#[tokio::test]
async fn raw_github_table_fans_out_in_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/pawrly/pawrly"))
        .respond_with(ResponseTemplate::new(200).set_body_string("repo-1"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/pawrly/melt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("repo-2"))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = SourceDef {
        name: "gh".into(),
        kind: SourceKind::Github,
        description: None,
        config: json!({
            "base_url": server.uri(),
            "token": "test-token"
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
        .sql(
            "SELECT response_status, response_body \
             FROM gh \
             WHERE request_path IN ('/repos/pawrly/pawrly', '/repos/pawrly/melt') \
             ORDER BY request_path",
        )
        .await
        .expect("plan");
    let batches = df.collect().await.expect("execute");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total, 2);
}

#[tokio::test]
async fn raw_table_rejects_unfiltered_scan() {
    let server = MockServer::start().await;
    let (ctx, catalog) = build_ctx().await;
    let def = SourceDef {
        name: "gh".into(),
        kind: SourceKind::Github,
        description: None,
        config: json!({
            "base_url": server.uri(),
            "token": "x"
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

    let df = ctx.sql("SELECT * FROM gh").await.expect("plan");
    let err = df.collect().await.expect_err("unfiltered raw should error");
    assert!(err.to_string().contains("PAWRLY_SAFETY_REQUIRED_FILTER"));
}
