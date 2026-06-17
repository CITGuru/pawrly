//! Acceptance for the `system.activity` table (activity log, sink 2): with the
//! table sink enabled, each engine operation lands a queryable row whose SQL is
//! redacted per policy.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;
use std::time::Duration;

use arrow_array::Array as _;
use pawrly_config::Config;
use pawrly_core::{EngineService, EngineServiceExt, QueryRequest};
use pawrly_engine::{LocalEngine, LocalEngineConfig};
use tempfile::TempDir;

fn cfg_yaml(yaml: &str) -> Config {
    let secrets = pawrly_secrets::StaticStore::new();
    pawrly_config::load_str(yaml, &secrets).unwrap()
}

async fn engine(workspace: &std::path::Path) -> Arc<dyn EngineService> {
    let yaml = r#"version: 1
observability:
  activity:
    enabled: true
    sinks: [table]
    redact_sql: literals
"#;
    Arc::new(
        LocalEngine::new(LocalEngineConfig {
            config: cfg_yaml(yaml),
            workspace_dir: workspace.to_path_buf(),
            duckdb_pool_size: None,
            home: None,
        })
        .await
        .unwrap(),
    )
}

/// Engine with a durable store at `store_dir`. `flush_threshold` controls when a
/// file is written (1 = every record); the flush timer is disabled.
async fn durable_engine(
    workspace: &std::path::Path,
    store_dir: &std::path::Path,
    flush_threshold: usize,
) -> Arc<dyn EngineService> {
    let yaml = format!(
        r#"version: 1
observability:
  activity:
    enabled: true
    sinks: [table]
    redact_sql: literals
    store: "{}"
    flush_threshold: {flush_threshold}
    flush_interval: 0s
"#,
        store_dir.display()
    );
    Arc::new(
        LocalEngine::new(LocalEngineConfig {
            config: cfg_yaml(&yaml),
            workspace_dir: workspace.to_path_buf(),
            duckdb_pool_size: None,
            home: None,
        })
        .await
        .unwrap(),
    )
}

/// Drain a query stream fully so its activity record is finalized.
async fn run(engine: &Arc<dyn EngineService>, sql: &str) {
    use futures_util::StreamExt as _;
    let mut stream = engine.query(QueryRequest::sql(sql)).await.unwrap();
    while let Some(item) = stream.next().await {
        item.unwrap();
    }
}

/// Poll `system.activity` until it returns a row (or give up), returning the count.
async fn activity_rows(engine: &Arc<dyn EngineService>) -> usize {
    for _ in 0..50 {
        let rows = engine
            .query_collect("SELECT * FROM system.activity")
            .await
            .unwrap();
        let total: usize = rows.iter().map(arrow_array::RecordBatch::num_rows).sum();
        if total > 0 {
            return total;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    0
}

#[tokio::test]
async fn recorded_query_is_queryable_via_system_activity() {
    let tmp = TempDir::new().unwrap();
    let engine = engine(tmp.path()).await;

    run(&engine, "SELECT 42 AS n WHERE 'topsecret' = 'topsecret'").await;

    // The record is drained on a background task; poll briefly for it.
    let mut rows = Vec::new();
    for _ in 0..50 {
        rows = engine
            .query_collect("SELECT operation, status, sql FROM system.activity")
            .await
            .unwrap();
        if rows
            .iter()
            .map(arrow_array::RecordBatch::num_rows)
            .sum::<usize>()
            > 0
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let total: usize = rows.iter().map(arrow_array::RecordBatch::num_rows).sum();
    assert!(
        total >= 1,
        "expected at least one activity row, got {total}"
    );

    // The recorded SQL is redacted: shape kept, literal dropped.
    let sql_col = rows[0]
        .column(2)
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    let recorded: Vec<&str> = (0..sql_col.len()).map(|i| sql_col.value(i)).collect();
    let joined = recorded.join(" | ");
    assert!(
        joined.contains("$REDACTED"),
        "expected redaction in: {joined}"
    );
    assert!(
        !joined.contains("topsecret"),
        "literal value leaked into system.activity: {joined}"
    );
}

#[tokio::test]
async fn durable_store_survives_a_restart() {
    let tmp = TempDir::new().unwrap();
    let store = tmp.path().join("activity");

    // First engine: run a query, confirm it's recorded.
    {
        let engine = durable_engine(tmp.path(), &store, 1).await;
        run(&engine, "SELECT 1 AS n").await;
        assert!(
            activity_rows(&engine).await >= 1,
            "record not visible in first engine"
        );
    }

    // A Parquet file was actually written under the partitioned layout.
    let parquet_count = walk_parquet(&store);
    assert!(
        parquet_count >= 1,
        "expected a Parquet file on disk, found {parquet_count}"
    );

    // Second engine over the same dir: the record persists across the restart.
    let engine = durable_engine(tmp.path(), &store, 1).await;
    assert!(
        activity_rows(&engine).await >= 1,
        "record did not survive restart"
    );
}

#[tokio::test]
async fn shutdown_flushes_sub_threshold_buffer() {
    let tmp = TempDir::new().unwrap();
    let store = tmp.path().join("activity");

    // High threshold + no timer: the lone record stays buffered, never reaching
    // disk except via the shutdown flush.
    {
        let engine = durable_engine(tmp.path(), &store, 1000).await;
        run(&engine, "SELECT 1 AS n").await;
        // Wait until the record is in the store (its buffer), still unflushed.
        assert!(activity_rows(&engine).await >= 1, "record not buffered");
        assert_eq!(walk_parquet(&store), 0, "nothing should be flushed yet");
        // Dropping the engine triggers the shutdown flush.
    }

    let engine = durable_engine(tmp.path(), &store, 1000).await;
    assert!(
        activity_rows(&engine).await >= 1,
        "buffered record was not flushed on shutdown"
    );
    assert!(
        walk_parquet(&store) >= 1,
        "shutdown flush wrote no Parquet file"
    );
}

/// Count `.parquet` files anywhere under `dir`.
fn walk_parquet(dir: &std::path::Path) -> usize {
    let mut count = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            count += walk_parquet(&path);
        } else if path.extension().is_some_and(|e| e == "parquet") {
            count += 1;
        }
    }
    count
}
