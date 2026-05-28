//! Acceptance: register an AI source against a wiremock OpenAI-compatible
//! endpoint, then issue a `SELECT chat(...)` SQL query.

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
use pawrly_sources_ai::register_ai_source;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_udf_against_wiremock_openai() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "the answer is 42"}}]
        })))
        .mount(&server)
        .await;

    let cfg = SessionConfig::new()
        .with_default_catalog_and_schema("pawrly", "default")
        .with_create_default_catalog_and_schema(false);
    let ctx = SessionContext::new_with_config(cfg);
    let catalog: Arc<MemoryCatalogProvider> = Arc::new(MemoryCatalogProvider::new());
    let _ = catalog
        .register_schema(
            "default",
            Arc::new(datafusion::catalog::MemorySchemaProvider::new()),
        )
        .unwrap();
    ctx.register_catalog("pawrly", catalog.clone());

    let def = SourceDef {
        name: "ai".into(),
        kind: SourceKind::Ai,
        description: None,
        config: json!({
            "provider": "openai",
            "base_url": server.uri(),
            "api_key": "test-key"
        }),
        cache: CachePolicy::None,
        safety: None,
        tables: Vec::new(),
        raw_table: false,
        raw_table_safety: None,
    };

    let report = register_ai_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register ai");
    assert_eq!(report.table_count, 1);
    assert!(report.udfs_registered.contains(&"ai.chat".to_string()));

    // Verify models table.
    let df = ctx
        .sql("SELECT name, model, provider FROM ai.models")
        .await
        .unwrap();
    let batches = df.collect().await.unwrap();
    assert_eq!(batches[0].num_rows(), 1);

    // Verify the chat UDF.
    let df = ctx
        .sql("SELECT \"ai.chat\"('gpt-5-mini', 'what is the meaning of life?') AS reply")
        .await
        .unwrap();
    let batches = df.collect().await.unwrap();
    let arr = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(arr.value(0), "the answer is 42");
}
