//! Acceptance: a `client_credentials` dynamic variable wired end to end through the engine.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use arrow_array::{Array, Int64Array, StringArray};
use pawrly_core::{EngineService, EngineServiceExt};
use pawrly_engine::{LocalEngine, LocalEngineConfig};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg_yaml(yaml: &str) -> pawrly_config::Config {
    let secrets = pawrly_secrets::StaticStore::new();
    pawrly_config::load_str(yaml, &secrets).expect("config parse")
}

fn scalar_i64(batches: &[arrow_array::RecordBatch]) -> i64 {
    let b = batches.first().expect("one batch");
    b.column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("i64")
        .value(0)
}

fn scalar_string(batches: &[arrow_array::RecordBatch]) -> String {
    let b = batches.first().expect("one batch");
    b.column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("utf8")
        .value(0)
        .to_string()
}

#[tokio::test]
async fn client_credentials_variable_end_to_end() {
    let idp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "minted-xyz", "expires_in": 3600 }),
            ),
        )
        .mount(&idp)
        .await;

    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("authorization", "Bearer minted-xyz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([ { "id": 7 } ])))
        .mount(&api)
        .await;

    let yaml = r#"version: 1
variables:
  API_TOKEN:
    kind: secret
    oauth:
      grant:
        type: client_credentials
      endpoints:
        token_url: __IDP__/token
      client:
        id: { default: cid }
        secret: { default: csec }
sources:
  - name: api
    kind: http
    config:
      base_url: __API__
      token: ${var:API_TOKEN}
    tables:
      - name: items
        endpoint: /items
        response:
          path: $
          schema:
            - { name: id, type: bigint }
"#
    .replace("__IDP__", &idp.uri())
    .replace("__API__", &api.uri());

    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&yaml),
        workspace_dir: std::env::temp_dir(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    let batches = svc
        .query_collect("SELECT id FROM api.items")
        .await
        .expect("query: the minted bearer should make the API mock match");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1, "expected one row from the authenticated API call");
}

#[tokio::test]
async fn device_code_variable_refreshes_at_runtime() {
    let idp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=stored-rt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "access_token": "refreshed-at", "expires_in": 3600 }),
        ))
        .mount(&idp)
        .await;

    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(header("authorization", "Bearer refreshed-at"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([ { "id": 9 } ])))
        .mount(&api)
        .await;

    let home = tempfile::tempdir().unwrap();
    let tokens: Arc<dyn pawrly_secrets::VariableValueStore> = Arc::new(
        pawrly_secrets::EncryptedFileTokenStore::new(home.path().join("variables")),
    );
    tokens
        .set(
            "root::GH_TOKEN",
            &pawrly_secrets::Secret::from("stored-rt".to_string()),
        )
        .unwrap();

    let yaml = r#"version: 1
variables:
  GH_TOKEN:
    kind: secret
    oauth:
      grant:
        type: device_code
      endpoints:
        device_authorization_url: __IDP__/device/code
        token_url: __IDP__/token
      client:
        id: { default: cid }
sources:
  - name: api
    kind: http
    config:
      base_url: __API__
      token: ${var:GH_TOKEN}
    tables:
      - name: items
        endpoint: /items
        response:
          path: $
          schema:
            - { name: id, type: bigint }
"#
    .replace("__IDP__", &idp.uri())
    .replace("__API__", &api.uri());

    let engine = LocalEngine::new_with_token_store(
        LocalEngineConfig {
            config: cfg_yaml(&yaml),
            workspace_dir: std::env::temp_dir(),
            duckdb_pool_size: None,
            home: Some(home.path().to_path_buf()),
        },
        tokens.clone(),
    )
    .await
    .expect("engine");
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    let batches = svc
        .query_collect("SELECT id FROM api.items")
        .await
        .expect("query: the refreshed bearer should make the API mock match");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        rows, 1,
        "expected one row from the refresh-authenticated call"
    );
}

#[tokio::test]
async fn system_variables_table_exposes_declarations() {
    let yaml = r#"version: 1
variables:
  API_BASE: { kind: variable, default: https://api.example.com, description: Base URL }
  PAGE_SIZE: { kind: variable, type: number, default: 100 }
sources:
  - name: gh
    kind: http
    variables:
      GH_TOKEN:
        kind: secret
        oauth:
          grant: { type: device_code }
          endpoints:
            device_authorization_url: https://gh/device/code
            token_url: https://gh/token
          client: { id: { default: cid } }
    config:
      base_url: https://example.com
      token: ${var:GH_TOKEN}
"#;
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(yaml),
        workspace_dir: std::env::temp_dir(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    let total = scalar_i64(
        &svc.query_collect("SELECT count(*) FROM system.variables")
            .await
            .expect("count"),
    );
    assert_eq!(total, 3, "API_BASE + PAGE_SIZE (global) + GH_TOKEN (gh)");

    let set = scalar_i64(
        &svc.query_collect("SELECT count(*) FROM system.variables WHERE available")
            .await
            .expect("count available"),
    );
    assert_eq!(set, 2);

    let page_type = scalar_string(
        &svc.query_collect("SELECT type FROM system.variables WHERE key = 'PAGE_SIZE'")
            .await
            .expect("type"),
    );
    assert_eq!(page_type, "number");
    let base_type = scalar_string(
        &svc.query_collect("SELECT type FROM system.variables WHERE key = 'API_BASE'")
            .await
            .expect("type"),
    );
    assert_eq!(base_type, "string");

    let required = scalar_i64(
        &svc.query_collect("SELECT count(*) FROM system.variables WHERE required")
            .await
            .expect("count required"),
    );
    assert_eq!(required, 1);

    let secret_key = scalar_string(
        &svc.query_collect("SELECT key FROM system.variables WHERE kind = 'secret'")
            .await
            .expect("secret key"),
    );
    assert_eq!(secret_key, "GH_TOKEN");

    let leaked = scalar_i64(
        &svc.query_collect(
            "SELECT count(*) FROM system.variables WHERE kind = 'secret' AND value IS NOT NULL",
        )
        .await
        .expect("leak check"),
    );
    assert_eq!(leaked, 0);
}
