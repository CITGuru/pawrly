//! Acceptance for the read-only namespace catalog: once a source table is
//! cached, the same snapshot is also addressable directly at
//! `<namespace>.<source>.<table>`, bypassing the live read-through wrapper. The
//! catalog is manifest-driven, so a source/table only appears once it has been
//! materialized to disk.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::path::PathBuf;
use std::sync::Arc;

use pawrly_config::Config;
use pawrly_core::{EngineService, EngineServiceExt};
use pawrly_engine::{LocalEngine, LocalEngineConfig};
use tempfile::TempDir;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("pawrly-cli")
        .join("tests")
        .join("fixtures")
}

fn cfg_yaml(yaml: &str) -> Config {
    let secrets = pawrly_secrets::StaticStore::new();
    pawrly_config::load_str(yaml, &secrets).unwrap()
}

/// The configured `storage` root (before the per-workspace namespace segment).
fn storage_root(workspace: &std::path::Path) -> PathBuf {
    workspace.join(".pawrly").join("cache")
}

/// Pins `namespace: test` so the second catalog is registered as `test` and the
/// on-disk cache path is deterministic.
fn orders_yaml(workspace: &std::path::Path) -> String {
    let parquet_path = fixtures_dir().join("orders.parquet");
    format!(
        r#"version: 1
defaults:
  cache:
    storage: "{}"
    namespace: test
sources:
  - name: data
    kind: file
    config:
      path: "{}"
    cache:
      mode: ttl
      ttl: 1h
    tables:
      - name: orders
        path: "{}"
        format: parquet
"#,
        storage_root(workspace).display(),
        fixtures_dir().display(),
        parquet_path.display(),
    )
}

async fn build_engine(workspace: &std::path::Path) -> Arc<dyn EngineService> {
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&orders_yaml(workspace)),
        workspace_dir: workspace.to_path_buf(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .unwrap();
    Arc::new(engine)
}

fn count_of(batches: &[arrow_array::RecordBatch]) -> i64 {
    batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .unwrap()
        .value(0)
}

#[tokio::test]
async fn namespace_catalog_serves_cached_snapshot_directly() {
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    // Before any cache write the snapshot does not exist: the manifest is empty,
    // so the `data` schema is absent from the `test` catalog.
    assert!(
        svc.query_collect("SELECT * FROM test.data.orders")
            .await
            .is_err(),
        "uncached snapshot must not resolve in the namespace catalog"
    );

    // Populate the cache via a normal live query (write-through to parquet).
    let live = svc
        .query_collect("SELECT COUNT(*) AS n FROM data.orders")
        .await
        .unwrap();
    assert_eq!(count_of(&live), 5);

    // Now the same data is addressable directly at `<namespace>.<source>.<table>`.
    let direct = svc
        .query_collect("SELECT COUNT(*) AS n FROM test.data.orders")
        .await
        .unwrap();
    assert_eq!(
        count_of(&direct),
        5,
        "direct namespace read should serve the cached snapshot"
    );

    // Projection through the snapshot provider works (not just COUNT(*)).
    let rows = svc
        .query_collect("SELECT * FROM test.data.orders")
        .await
        .unwrap();
    let total: usize = rows.iter().map(arrow_array::RecordBatch::num_rows).sum();
    assert_eq!(total, 5, "projected scan should return every cached row");

    // A table that was never cached does not resolve, even under a known source.
    assert!(
        svc.query_collect("SELECT * FROM test.data.ghost")
            .await
            .is_err(),
        "uncached table must not resolve"
    );
}

/// No `storage:` / `namespace:` in config and `home` == the workspace dir: the
/// default workspace. Storage must derive to `<home>/cache`, the namespace must
/// be the literal `default`, and registering a catalog named `default` must not
/// break unqualified resolution against the session's `pawrly.default` schema.
#[tokio::test]
async fn default_workspace_uses_home_cache_and_default_namespace() {
    let home = TempDir::new().unwrap();
    let parquet_path = fixtures_dir().join("orders.parquet");
    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{}"
    cache:
      mode: ttl
      ttl: 1h
    tables:
      - name: orders
        path: "{}"
        format: parquet
"#,
        fixtures_dir().display(),
        parquet_path.display(),
    );
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&yaml),
        workspace_dir: home.path().to_path_buf(),
        duckdb_pool_size: None,
        home: Some(home.path().to_path_buf()),
    })
    .await
    .unwrap();
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    // Live query still resolves unqualified (`pawrly.default` schema intact).
    let live = svc
        .query_collect("SELECT COUNT(*) AS n FROM data.orders")
        .await
        .unwrap();
    assert_eq!(count_of(&live), 5);

    // The snapshot is addressable under the `default` namespace catalog.
    let direct = svc
        .query_collect("SELECT COUNT(*) AS n FROM default.data.orders")
        .await
        .unwrap();
    assert_eq!(count_of(&direct), 5);

    // The cache landed under `<home>/cache/default/`.
    let cache_dir = home.path().join("cache").join("default");
    assert!(
        cache_dir.is_dir(),
        "expected derived cache root at {}",
        cache_dir.display()
    );
}

#[tokio::test]
async fn namespace_catalog_reflects_manifest_across_restart() {
    let workspace = TempDir::new().unwrap();

    // First engine: populate the cache, then drop it.
    {
        let svc = build_engine(workspace.path()).await;
        svc.query_collect("SELECT COUNT(*) AS n FROM data.orders")
            .await
            .unwrap();
    }

    // A fresh engine over the same workspace re-reads the manifest from disk, so
    // the snapshot is directly addressable immediately on startup — no live
    // query needed first.
    let svc2 = build_engine(workspace.path()).await;
    let direct = svc2
        .query_collect("SELECT COUNT(*) AS n FROM test.data.orders")
        .await
        .unwrap();
    assert_eq!(count_of(&direct), 5);
}
