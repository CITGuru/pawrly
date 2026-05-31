//! Acceptance: `kind: file` table options — a glob/directory unioned into one
//! table, CSV dialect overrides with an explicit schema, and hive-partitioned
//! datasets exposing partition columns.

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
use pawrly_core::{CachePolicy, SourceDef, SourceKind, TableDef};
use pawrly_sources_duckdb::register_file_source;
use serde_json::{Value, json};

async fn build_ctx() -> (SessionContext, Arc<MemoryCatalogProvider>) {
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
    (ctx, catalog)
}

fn file_source(table_config: Value) -> SourceDef {
    SourceDef {
        name: "data".into(),
        kind: SourceKind::File,
        description: None,
        config: json!({}),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![TableDef {
            name: "t".into(),
            description: None,
            config: table_config,
            cache: None,
            safety: None,
        }],
        raw_table: false,
        raw_table_safety: None,
    }
}

async fn count(ctx: &SessionContext, sql: &str) -> i64 {
    let batches = ctx.sql(sql).await.expect("plan").collect().await.expect("execute");
    batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<datafusion::arrow::array::Int64Array>()
        .unwrap()
        .value(0)
}

/// A glob `path` unions every matching file into one table.
#[tokio::test]
async fn glob_unions_into_one_table() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("a.csv"), "id\n1\n2\n").unwrap();
    std::fs::write(data.join("b.csv"), "id\n3\n").unwrap();

    let (ctx, catalog) = build_ctx().await;
    let def = file_source(json!({ "path": "data/*.csv", "format": "csv" }));
    register_file_source(&def, &ctx, catalog.as_ref(), dir.path())
        .await
        .expect("register glob table");

    assert_eq!(count(&ctx, "SELECT count(*) FROM data.t").await, 3);
}

/// CSV dialect overrides (tab delimiter, no header) plus an explicit schema name
/// and type the columns of a headerless file.
#[tokio::test]
async fn csv_options_with_explicit_schema() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("metrics.tsv"), "alpha\t10\nbeta\t20\n").unwrap();

    let (ctx, catalog) = build_ctx().await;
    let def = file_source(json!({
        "path": "metrics.tsv",
        "format": "csv",
        "csv": { "header": false, "delimiter": "\t" },
        "schema": [
            { "name": "host",  "type": "varchar" },
            { "name": "value", "type": "bigint" }
        ]
    }));
    register_file_source(&def, &ctx, catalog.as_ref(), dir.path())
        .await
        .expect("register csv table");

    // Sums the typed `value` column; proves the headerless rows + schema parsed.
    assert_eq!(
        count(&ctx, "SELECT sum(value) FROM data.t WHERE host = 'beta'").await,
        20
    );
}

/// A hive-partitioned directory exposes `dt` as a queryable, prunable column.
#[tokio::test]
async fn hive_partition_column_is_queryable() {
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("events").join("dt=2026-05-31");
    std::fs::create_dir_all(&part).unwrap();
    std::fs::write(part.join("part.csv"), "id\n1\n2\n").unwrap();
    let other = dir.path().join("events").join("dt=2026-05-30");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("part.csv"), "id\n9\n").unwrap();

    let (ctx, catalog) = build_ctx().await;
    let def = file_source(json!({
        "path": "events",
        "format": "csv",
        "partition_cols": [ { "name": "dt", "type": "varchar" } ]
    }));
    register_file_source(&def, &ctx, catalog.as_ref(), dir.path())
        .await
        .expect("register partitioned table");

    assert_eq!(
        count(
            &ctx,
            "SELECT count(*) FROM data.t WHERE dt = '2026-05-31'"
        )
        .await,
        2
    );
}
