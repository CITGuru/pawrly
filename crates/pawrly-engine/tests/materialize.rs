//! Acceptance for the `materialize` write verb: a query result (or file / url /
//! inline) is persisted as a pinned, self-backed table addressable as
//! `<namespace>.materialized.<name>`, surfaced through the namespace catalog,
//! immune to vacuum, and replaceable / droppable by name.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use pawrly_config::Config;
use pawrly_core::{EngineService, EngineServiceExt, MaterializeSpec, TableName};
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

fn storage_root(workspace: &std::path::Path) -> PathBuf {
    workspace.join(".pawrly").join("cache")
}

/// Pins `namespace: test`, so materialized tables are addressable as
/// `test.materialized.<name>`. The `data` source is *not* cached — materialize
/// must work over a live source, not only over cached snapshots.
fn orders_yaml(workspace: &std::path::Path) -> String {
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
    tables:
      - name: orders
        path: "{}"
        format: parquet
"#,
        storage_root(workspace).display(),
        fixtures_dir().display(),
        fixtures_dir().join("orders.parquet").display(),
    )
}

async fn build_engine(workspace: &std::path::Path) -> Arc<dyn EngineService> {
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&orders_yaml(workspace)),
        workspace_dir: workspace.to_path_buf(),
        duckdb_pool_size: None,
    })
    .await
    .unwrap();
    Arc::new(engine)
}

fn query(sql: impl Into<String>) -> MaterializeSpec {
    MaterializeSpec::Query {
        sql: sql.into(),
        params: HashMap::new(),
    }
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
async fn materialize_query_persists_and_is_addressable() {
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    let outcome = svc
        .materialize(
            "rev_by_customer",
            query("SELECT customer, SUM(amount_cents) AS total FROM data.orders GROUP BY customer"),
        )
        .await
        .unwrap();

    // The outcome carries the artifact: fq-name, a real file, the row count.
    assert_eq!(
        outcome.name,
        TableName::new("materialized", "rev_by_customer")
    );
    assert_eq!(outcome.row_count, 4);
    assert!(outcome.size_bytes > 0);
    assert!(outcome.file_path.exists(), "parquet artifact must exist");
    assert!(
        outcome
            .file_path
            .starts_with(storage_root(workspace.path())),
        "artifact must live under the cache root"
    );

    // Addressable by name through the namespace catalog.
    let rows = svc
        .query_collect("SELECT COUNT(*) AS n FROM test.materialized.rev_by_customer")
        .await
        .unwrap();
    assert_eq!(count_of(&rows), 4);

    // Real data, not just a row count: ben = 2500 + 4200 = 6700.
    let ben = svc
        .query_collect("SELECT total FROM test.materialized.rev_by_customer WHERE customer = 'ben'")
        .await
        .unwrap();
    assert_eq!(count_of(&ben), 6700);

    // It shows up as a cache entry (so `pawrly cache list` surfaces it).
    let entries = svc.cache_entries().await.unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.name.to_string() == "materialized.rev_by_customer"),
        "materialized table should appear in cache entries"
    );
}

#[tokio::test]
async fn materialized_table_is_pinned_against_vacuum() {
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    let outcome = svc
        .materialize("snap", query("SELECT * FROM data.orders"))
        .await
        .unwrap();
    assert!(outcome.file_path.exists());

    // Vacuum must not reclaim a self-backed table — it has no upstream to refetch.
    svc.vacuum_cache().await.unwrap();

    assert!(
        outcome.file_path.exists(),
        "vacuum must not delete a materialized artifact"
    );
    let rows = svc
        .query_collect("SELECT COUNT(*) AS n FROM test.materialized.snap")
        .await
        .unwrap();
    assert_eq!(count_of(&rows), 5);
}

#[tokio::test]
async fn materialize_is_create_or_replace_and_droppable() {
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    // First version: all 5 rows.
    svc.materialize("t", query("SELECT * FROM data.orders"))
        .await
        .unwrap();
    let v1 = svc
        .query_collect("SELECT COUNT(*) AS n FROM test.materialized.t")
        .await
        .unwrap();
    assert_eq!(count_of(&v1), 5);

    // Replace by name: now only 2 rows.
    let replaced = svc
        .materialize(
            "t",
            query("SELECT * FROM data.orders WHERE amount_cents > 1000"),
        )
        .await
        .unwrap();
    assert_eq!(replaced.row_count, 2);
    let v2 = svc
        .query_collect("SELECT COUNT(*) AS n FROM test.materialized.t")
        .await
        .unwrap();
    assert_eq!(count_of(&v2), 2, "re-materialize must overwrite by name");

    // Drop it.
    assert!(svc.drop_materialized("t").await.unwrap());
    assert!(
        svc.query_collect("SELECT * FROM test.materialized.t")
            .await
            .is_err(),
        "dropped table must no longer resolve"
    );
    // Dropping again is a no-op.
    assert!(!svc.drop_materialized("t").await.unwrap());
}

#[tokio::test]
async fn materialize_survives_restart() {
    let workspace = TempDir::new().unwrap();
    {
        let svc = build_engine(workspace.path()).await;
        svc.materialize("keep", query("SELECT * FROM data.orders"))
            .await
            .unwrap();
    }
    // A fresh engine reads the pinned entry from the on-disk manifest.
    let svc2 = build_engine(workspace.path()).await;
    let rows = svc2
        .query_collect("SELECT COUNT(*) AS n FROM test.materialized.keep")
        .await
        .unwrap();
    assert_eq!(count_of(&rows), 5);
}

#[tokio::test]
async fn materialize_from_local_file_infers_format() {
    use pawrly_core::MaterializeFormat;

    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    // Format inferred from the `.parquet` extension; absolute path.
    let outcome = svc
        .materialize(
            "from_file",
            MaterializeSpec::File {
                path: fixtures_dir().join("orders.parquet"),
                format: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome.row_count, 5);

    let rows = svc
        .query_collect("SELECT COUNT(*) AS n FROM test.materialized.from_file")
        .await
        .unwrap();
    assert_eq!(count_of(&rows), 5);

    // Explicit format also works (CSV fixture).
    let csv = svc
        .materialize(
            "from_csv",
            MaterializeSpec::File {
                path: fixtures_dir().join("customers.csv"),
                format: Some(MaterializeFormat::Csv),
            },
        )
        .await
        .unwrap();
    assert!(csv.row_count >= 1, "csv fixture should have rows");
}

#[tokio::test]
async fn materialize_from_inline_bytes() {
    use pawrly_core::MaterializeFormat;

    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    let outcome = svc
        .materialize(
            "inline",
            MaterializeSpec::Inline {
                bytes: b"city,pop\nlagos,15000000\nabuja,3000000\n".to_vec(),
                format: MaterializeFormat::Csv,
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome.row_count, 2);

    let rows = svc
        .query_collect("SELECT pop FROM test.materialized.inline WHERE city = 'lagos'")
        .await
        .unwrap();
    assert_eq!(count_of(&rows), 15_000_000);
}

#[tokio::test]
async fn refresh_reruns_materialized_origin() {
    use pawrly_core::MaterializeFormat;

    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    // Materialize from a file we control, then change the file underneath.
    let src = workspace.path().join("src.csv");
    std::fs::write(&src, "city,pop\nlagos,1\n").unwrap();
    let out = svc
        .materialize(
            "cities",
            MaterializeSpec::File {
                path: src.clone(),
                format: Some(MaterializeFormat::Csv),
            },
        )
        .await
        .unwrap();
    assert_eq!(out.row_count, 1);

    // Grow the source file, then refresh by name — it re-runs the stored origin.
    std::fs::write(&src, "city,pop\nlagos,1\nabuja,2\nkano,3\n").unwrap();
    let refreshed = svc
        .refresh_table(&TableName::new("materialized", "cities"))
        .await
        .unwrap();
    assert_eq!(refreshed.rows_written, 3, "refresh must re-read the file");

    let rows = svc
        .query_collect("SELECT COUNT(*) AS n FROM test.materialized.cities")
        .await
        .unwrap();
    assert_eq!(count_of(&rows), 3);

    // Refreshing a non-existent materialized table errors.
    assert!(
        svc.refresh_table(&TableName::new("materialized", "ghost"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unqualified_materialized_name_resolves() {
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;
    svc.materialize("t", query("SELECT * FROM data.orders"))
        .await
        .unwrap();

    // Reachable both with and without the namespace prefix.
    let unq = svc
        .query_collect("SELECT COUNT(*) AS n FROM materialized.t")
        .await
        .unwrap();
    assert_eq!(count_of(&unq), 5);
}

#[tokio::test]
async fn inline_directive_persists_and_returns_rows() {
    let workspace = TempDir::new().unwrap();
    let yaml = format!(
        r#"version: 1
defaults:
  cache:
    storage: "{}"
    namespace: test
  materialize:
    allow_inline: true
sources:
  - name: data
    kind: file
    config:
      path: "{}"
    tables:
      - name: orders
        path: "{}"
        format: parquet
"#,
        storage_root(workspace.path()).display(),
        fixtures_dir().display(),
        fixtures_dir().join("orders.parquet").display(),
    );
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&yaml),
        workspace_dir: workspace.path().to_path_buf(),
        duckdb_pool_size: None,
    })
    .await
    .unwrap();
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    // The directive persists the result and still returns the rows.
    let rows = svc
        .query_collect(
            "-- pawrly: materialize big_orders\nSELECT * FROM data.orders WHERE amount_cents > 1000",
        )
        .await
        .unwrap();
    let returned: usize = rows.iter().map(arrow_array::RecordBatch::num_rows).sum();
    assert_eq!(returned, 2, "the query still returns its rows");

    // And the table now exists.
    let persisted = svc
        .query_collect("SELECT COUNT(*) AS n FROM materialized.big_orders")
        .await
        .unwrap();
    assert_eq!(count_of(&persisted), 2);
}

#[tokio::test]
async fn inline_directive_off_by_default() {
    // Without allow_inline, the directive is an inert SQL comment.
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;
    svc.query_collect("-- pawrly: materialize nope\nSELECT * FROM data.orders")
        .await
        .unwrap();
    assert!(
        svc.query_collect("SELECT * FROM materialized.nope")
            .await
            .is_err(),
        "directive must not fire when allow_inline is off"
    );
}

#[tokio::test]
async fn invalid_materialized_name_is_rejected() {
    let workspace = TempDir::new().unwrap();
    let svc = build_engine(workspace.path()).await;

    for bad in ["", "a.b", "a/b", "has space", " leading"] {
        assert!(
            svc.materialize(bad, query("SELECT 1")).await.is_err(),
            "name `{bad}` should be rejected"
        );
    }
}
