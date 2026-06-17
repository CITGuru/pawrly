//! Acceptance for the `system.activity` table (activity log, sink 2): with the
//! table sink enabled, each engine operation lands a queryable row whose SQL is
//! redacted per policy.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "tests")]

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

/// Drain a query stream fully so its activity record is finalized.
async fn run(engine: &Arc<dyn EngineService>, sql: &str) {
    use futures_util::StreamExt as _;
    let mut stream = engine.query(QueryRequest::sql(sql)).await.unwrap();
    while let Some(item) = stream.next().await {
        item.unwrap();
    }
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
        if rows.iter().map(arrow_array::RecordBatch::num_rows).sum::<usize>() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let total: usize = rows.iter().map(arrow_array::RecordBatch::num_rows).sum();
    assert!(total >= 1, "expected at least one activity row, got {total}");

    // The recorded SQL is redacted: shape kept, literal dropped.
    let sql_col = rows[0]
        .column(2)
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    let recorded: Vec<&str> = (0..sql_col.len()).map(|i| sql_col.value(i)).collect();
    let joined = recorded.join(" | ");
    assert!(joined.contains("$REDACTED"), "expected redaction in: {joined}");
    assert!(
        !joined.contains("topsecret"),
        "literal value leaked into system.activity: {joined}"
    );
}
