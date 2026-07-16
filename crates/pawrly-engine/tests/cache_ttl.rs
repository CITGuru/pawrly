//! Acceptance: TTL caching writes a parquet file to disk, the manifest
//! records it, and a fresh engine over the same workspace finds the cached
//! entry on startup (proving restart-safety).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::path::PathBuf;
use std::sync::Arc;

use pawrly_config::Config;
use pawrly_core::{EngineService, EngineServiceExt, TableName};
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

/// The configured `storage` root (before the per-workspace namespace segment).
fn storage_root(workspace: &std::path::Path) -> PathBuf {
    workspace.join(".pawrly").join("cache")
}

/// The actual cache root on disk: `storage/<namespace>`. These tests pin
/// `namespace: test` so the path is deterministic.
fn cache_root(workspace: &std::path::Path) -> PathBuf {
    storage_root(workspace).join("test")
}

fn orders_cache_file(workspace: &std::path::Path) -> PathBuf {
    cache_root(workspace)
        .join("data")
        .join("data")
        .join("orders")
        .join("part-000000.parquet")
}

#[tokio::test]
async fn ttl_cache_round_trip_and_restart() {
    let workspace = TempDir::new().unwrap();
    let parquet_path = fixtures_dir().join("orders.parquet");

    let yaml = format!(
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
        storage_root(workspace.path()).display(),
        fixtures_dir().display(),
        parquet_path.display(),
    );

    // 1. First engine: cache miss → live fetch → write through to parquet.
    {
        let engine = LocalEngine::new(LocalEngineConfig {
            config: cfg_yaml(&yaml),
            workspace_dir: workspace.path().to_path_buf(),
            duckdb_pool_size: None,
            home: None,
        })
        .await
        .unwrap();
        let svc: Arc<dyn EngineService> = Arc::new(engine);
        let batches = svc
            .query_collect("SELECT COUNT(*) AS n FROM data.orders")
            .await
            .unwrap();
        let arr = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<arrow_array::Int64Array>()
            .unwrap();
        assert_eq!(arr.value(0), 5);

        let entries = svc.cache_entries(None).await.unwrap();
        assert_eq!(entries.len(), 1, "expected 1 cache entry after first query");
        assert_eq!(entries[0].name.to_string(), "data.orders");
        assert_eq!(entries[0].row_count, 5);
    }

    // 2. The cache file is on disk.
    let cache_file = orders_cache_file(workspace.path());
    assert!(
        cache_file.exists(),
        "cache file should exist at {}",
        cache_file.display()
    );

    // 3. The manifest is on disk.
    let manifest = cache_root(workspace.path()).join("manifest.json");
    assert!(manifest.exists(), "manifest.json should exist");
    let manifest_text = std::fs::read_to_string(&manifest).unwrap();
    assert!(manifest_text.contains("data"));
    assert!(manifest_text.contains("orders"));

    // 4. A fresh engine over the same workspace finds the cached entry on startup.
    let engine2 = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&yaml),
        workspace_dir: workspace.path().to_path_buf(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .unwrap();
    let svc2: Arc<dyn EngineService> = Arc::new(engine2);
    let entries2 = svc2.cache_entries(None).await.unwrap();
    assert_eq!(
        entries2.len(),
        1,
        "fresh engine should re-load the manifest from disk"
    );
    assert_eq!(entries2[0].row_count, 5);

    // And queries still work end-to-end.
    let batches = svc2
        .query_collect("SELECT COUNT(*) AS n FROM data.orders")
        .await
        .unwrap();
    let arr = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .unwrap();
    assert_eq!(arr.value(0), 5);
}

#[tokio::test]
async fn refresh_table_writes_through() {
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    // No query yet → cache is empty.
    assert!(svc.cache_entries(None).await.unwrap().is_empty());

    let name = TableName::new("data", "orders");
    let out = svc.refresh_table(&name).await.unwrap();
    assert_eq!(out.rows_written, 5);
    assert!(out.size_bytes > 0, "expected a non-empty parquet write");

    let entries = svc.cache_entries(None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].row_count, 5);
    assert!(
        orders_cache_file(workspace.path()).exists(),
        "refresh should write the parquet file"
    );

    // Refreshing an unknown table is an error.
    assert!(
        svc.refresh_table(&TableName::new("data", "ghost"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn invalidate_removes_entry_and_file() {
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    svc.query_collect("SELECT COUNT(*) AS n FROM data.orders")
        .await
        .unwrap();
    let cache_file = orders_cache_file(workspace.path());
    assert!(cache_file.exists());
    assert_eq!(svc.cache_entries(None).await.unwrap().len(), 1);

    let name = TableName::new("data", "orders");
    assert!(svc.invalidate_cache(&name).await.unwrap());
    assert!(!cache_file.exists(), "invalidate should delete the file");
    assert!(svc.cache_entries(None).await.unwrap().is_empty());

    // Invalidating again reports nothing to remove.
    assert!(!svc.invalidate_cache(&name).await.unwrap());
}

#[tokio::test]
async fn vacuum_removes_orphans_keeps_live_and_recent_tmp() {
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    svc.query_collect("SELECT COUNT(*) AS n FROM data.orders")
        .await
        .unwrap();
    let live = orders_cache_file(workspace.path());
    assert!(live.exists());

    // An orphaned data file not referenced by the manifest.
    let root = cache_root(workspace.path());
    let orphan_dir = root.join("data").join("ghost").join("table");
    std::fs::create_dir_all(&orphan_dir).unwrap();
    let orphan = orphan_dir.join("part-000000.parquet");
    std::fs::write(&orphan, b"junk-bytes").unwrap();

    // A freshly written tmp file should be preserved (only >1h old is reclaimed).
    let tmp_dir = root.join("tmp");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let recent_tmp = tmp_dir.join("recent.parquet");
    std::fs::write(&recent_tmp, b"in-progress").unwrap();

    let report = svc.vacuum_cache().await.unwrap();

    assert!(!orphan.exists(), "orphan should be removed");
    assert!(live.exists(), "live cache file must survive vacuum");
    assert!(recent_tmp.exists(), "recent tmp must be preserved");
    assert!(report.files_removed >= 1);
    assert!(report.bytes_reclaimed >= 10);
    assert_eq!(
        svc.cache_entries(None).await.unwrap().len(),
        1,
        "live entry must remain in the manifest"
    );
}

#[tokio::test]
async fn corrupt_cache_file_self_heals() {
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    // Populate the cache.
    let count = |svc: &Arc<dyn EngineService>| {
        let svc = svc.clone();
        async move {
            let batches = svc
                .query_collect("SELECT COUNT(*) AS n FROM data.orders")
                .await
                .unwrap();
            batches[0]
                .column(0)
                .as_any()
                .downcast_ref::<arrow_array::Int64Array>()
                .unwrap()
                .value(0)
        }
    };
    assert_eq!(count(&svc).await, 5);

    // Corrupt the cached parquet file on disk.
    let cache_file = orders_cache_file(workspace.path());
    assert!(cache_file.exists());
    std::fs::write(&cache_file, b"definitely not parquet").unwrap();

    // The next query detects the corruption, quarantines the file, re-fetches
    // live, and returns the correct result — no error surfaces.
    assert_eq!(count(&svc).await, 5);

    // The bad file was moved under corrupt/ and a fresh cache file rewritten.
    let corrupt_dir = cache_root(workspace.path())
        .join("corrupt")
        .join("data")
        .join("orders");
    assert!(
        corrupt_dir.exists() && std::fs::read_dir(&corrupt_dir).unwrap().count() >= 1,
        "corrupt file should be quarantined"
    );
    assert!(
        cache_file.exists(),
        "a fresh cache file should be rewritten"
    );
    assert_eq!(svc.cache_entries(None).await.unwrap().len(), 1);
}

#[tokio::test]
async fn write_through_is_atomic_no_leftover_tmp() {
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    svc.query_collect("SELECT COUNT(*) AS n FROM data.orders")
        .await
        .unwrap();

    let root = cache_root(workspace.path());
    let tmp_dir = root.join("tmp");
    if tmp_dir.exists() {
        let leftover = std::fs::read_dir(&tmp_dir).unwrap().count();
        assert_eq!(leftover, 0, "tmp should be empty after a successful write");
    }

    let manifest = std::fs::read_to_string(root.join("manifest.json")).unwrap();
    assert!(
        manifest.contains("\"orders\""),
        "manifest must record the entry"
    );
    // A torn write would leave the temp file in place; the rename target must be present.
    assert!(root.join("manifest.json").exists());
}
