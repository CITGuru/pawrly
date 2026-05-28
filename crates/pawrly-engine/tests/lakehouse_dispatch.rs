//! Acceptance: lakehouse / warehouse kinds (`snowflake`, `iceberg`, `delta`,
//! `s3`, `gcs`, `azure`) are recognized by the engine and produce a clear
//! "requires lakehouse feature" error rather than panicking. The dispatch
//! table is in place; turning the backends on is a build-feature choice.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use pawrly_config::Config;
use pawrly_engine::{LocalEngine, LocalEngineConfig};
use tempfile::TempDir;

fn cfg_yaml(yaml: &str) -> Config {
    let secrets = pawrly_secrets::StaticStore::new();
    pawrly_config::load_str(yaml, &secrets).unwrap()
}

#[tokio::test]
async fn lakehouse_kinds_return_clear_feature_gated_errors() {
    let workspace = TempDir::new().unwrap();
    for kind in ["snowflake", "iceberg", "delta", "s3", "gcs", "azure"] {
        let yaml = format!(
            r#"version: 1
sources:
  - name: warehouse
    kind: {kind}
    config:
      account: test
"#
        );
        let cfg = cfg_yaml(&yaml);
        let res = LocalEngine::new(LocalEngineConfig {
            config: cfg,
            workspace_dir: workspace.path().to_path_buf(),
            duckdb_pool_size: None,
        })
        .await;
        let err = res.expect_err(&format!("kind `{kind}` should error at registration"));
        let s = err.to_string();
        assert!(
            s.contains("lakehouse"),
            "kind `{kind}` error should mention `lakehouse` feature; got: {s}"
        );
    }
}

#[tokio::test]
async fn postgres_mysql_recognized_but_feature_gated() {
    let workspace = TempDir::new().unwrap();
    for kind in ["postgres", "mysql"] {
        let yaml = format!(
            r#"version: 1
sources:
  - name: db
    kind: {kind}
    config:
      dsn: postgres://localhost/x
"#
        );
        let cfg = cfg_yaml(&yaml);
        let res = LocalEngine::new(LocalEngineConfig {
            config: cfg,
            workspace_dir: workspace.path().to_path_buf(),
            duckdb_pool_size: None,
        })
        .await;
        let err = res.expect_err(&format!("kind `{kind}` should error"));
        let s = err.to_string();
        assert!(
            s.contains("duckdb-extensions") || s.contains("sqlite"),
            "kind `{kind}` error: {s}"
        );
    }
}

#[tokio::test]
async fn sqlite_still_works() {
    let workspace = TempDir::new().unwrap();
    let f = tempfile::NamedTempFile::new().unwrap();
    let conn = rusqlite::Connection::open(f.path()).unwrap();
    conn.execute_batch("CREATE TABLE x (id INTEGER); INSERT INTO x VALUES (1),(2);")
        .unwrap();
    drop(conn);

    let yaml = format!(
        r#"version: 1
sources:
  - name: db
    kind: sqlite
    config:
      path: "{}"
"#,
        f.path().display()
    );
    let cfg = cfg_yaml(&yaml);
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: workspace.path().to_path_buf(),
        duckdb_pool_size: None,
    })
    .await
    .unwrap();
    use pawrly_core::EngineServiceExt;
    let batches = engine
        .query_collect("SELECT count(*) AS n FROM db.x")
        .await
        .unwrap();
    let arr = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .unwrap();
    assert_eq!(arr.value(0), 2);
}
