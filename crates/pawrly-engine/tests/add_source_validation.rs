//! Runtime `add_source` runs config-file validation plus the no-stdio rule
//! (a remotely added source must not spawn host processes).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use pawrly_core::{EngineService, SourceDef, SourceKind};
use pawrly_engine::{LocalEngine, LocalEngineConfig};

async fn empty_engine() -> (Arc<dyn EngineService>, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().unwrap();
    let secrets = pawrly_secrets::StaticStore::new();
    let cfg = pawrly_config::load_str("version: 1\nsources: []\n", &secrets).unwrap();
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: tmp.path().to_path_buf(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .unwrap();
    (Arc::new(engine), tmp)
}

fn source(name: &str, kind: SourceKind, config: serde_json::Value) -> SourceDef {
    SourceDef {
        name: name.into(),
        kind,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config,
        cache: Default::default(),
        safety: None,
        tables: Vec::new(),
        raw_table: false,
        raw_table_safety: None,
    }
}

#[tokio::test]
async fn mcp_stdio_is_rejected_at_runtime() {
    let (svc, _tmp) = empty_engine().await;
    let err = svc
        .add_source(source(
            "evil",
            SourceKind::Mcp,
            serde_json::json!({ "transport": "stdio", "command": "/bin/sh" }),
        ))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("cannot be added at runtime"),
        "{err}"
    );
}

#[tokio::test]
async fn plaintext_mcp_http_is_rejected() {
    let (svc, _tmp) = empty_engine().await;
    let err = svc
        .add_source(source(
            "plain",
            SourceKind::Mcp,
            serde_json::json!({ "transport": "streamable_http", "url": "http://internal.corp/mcp" }),
        ))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("must use https"), "{err}");
}

#[tokio::test]
async fn invalid_name_is_rejected() {
    let (svc, _tmp) = empty_engine().await;
    let err = svc
        .add_source(source(
            "bad name!",
            SourceKind::File,
            serde_json::json!({ "path": "./x.csv" }),
        ))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("valid SQL identifier"), "{err}");
}

#[tokio::test]
async fn valid_file_source_still_registers() {
    let (svc, tmp) = empty_engine().await;
    std::fs::write(tmp.path().join("rows.csv"), "id\n1\n").unwrap();
    let info = svc
        .add_source(source(
            "data",
            SourceKind::File,
            serde_json::json!({ "path": tmp.path().join("*.csv").display().to_string() }),
        ))
        .await
        .expect("valid source registers");
    assert_eq!(info.name, "data");
}
