//! Acceptance: a dynamic `${var:}` credential is minted through the variable store and attached per request.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::collections::HashMap;
use std::sync::Arc;

use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider};
use datafusion::execution::config::SessionConfig;
use datafusion::execution::context::SessionContext;
use pawrly_core::{CachePolicy, DynamicVarSpec, SourceDef, SourceKind, TableDef, TokenTransport};
use pawrly_secrets::{RuntimeVariableStore, VariableStore};
use pawrly_sources_http::register_http_source_with_vars;
use serde_json::json;
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

fn items_def(base_url: String) -> SourceDef {
    SourceDef {
        name: "api".into(),
        kind: SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: json!({ "base_url": base_url, "token": "${var:API_TOKEN}" }),
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
    }
}

fn store_for(token_url: String) -> Arc<dyn VariableStore> {
    let mut specs = HashMap::new();
    specs.insert(
        "api::API_TOKEN".to_string(),
        DynamicVarSpec::ClientCredentials {
            endpoints: pawrly_core::Endpoints::token(token_url),
            client_id: "cid".into(),
            client_secret: "csec".into(),
            scope: None,
            audience: None,
            transport: TokenTransport::RequestBody,
        },
    );
    Arc::new(RuntimeVariableStore::new(specs))
}

fn dynamic_map() -> HashMap<String, String> {
    HashMap::from([("API_TOKEN".to_string(), "api::API_TOKEN".to_string())])
}

async fn query_rows(ctx: &SessionContext) -> usize {
    ctx.sql("SELECT id FROM api.items")
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute: the minted bearer should make the API mock match")
        .iter()
        .map(|b| b.num_rows())
        .sum()
}

#[tokio::test]
async fn client_credentials_token_minted_and_attached() {
    let idp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "access_token": "minted-tok", "expires_in": 3600 })),
        )
        .mount(&idp)
        .await;

    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("authorization", "Bearer minted-tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&api)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let store = store_for(format!("{}/token", idp.uri()));
    register_http_source_with_vars(
        &items_def(api.uri()),
        &ctx,
        catalog.as_ref(),
        &store,
        dynamic_map(),
    )
    .await
    .expect("register");

    assert_eq!(query_rows(&ctx).await, 1);
}

#[tokio::test]
async fn minted_token_is_cached_across_queries() {
    let idp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "access_token": "minted-tok", "expires_in": 3600 })),
        )
        .expect(1)
        .mount(&idp)
        .await;

    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("authorization", "Bearer minted-tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ { "id": 1 } ])))
        .mount(&api)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let store = store_for(format!("{}/token", idp.uri()));
    register_http_source_with_vars(
        &items_def(api.uri()),
        &ctx,
        catalog.as_ref(),
        &store,
        dynamic_map(),
    )
    .await
    .expect("register");

    assert_eq!(query_rows(&ctx).await, 1);
    assert_eq!(query_rows(&ctx).await, 1);
    // `idp`'s `.expect(1)` is asserted on drop.
}
