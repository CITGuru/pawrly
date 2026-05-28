//! Acceptance: SQLite source with WHERE-equality predicate pushdown,
//! tested against an in-process SQLite database.

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
use pawrly_sources_duckdb::register_sqlite_source;
use serde_json::json;

/// Build a SessionContext rooted at the `pawrly.default` catalog/schema.
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

/// Seed an on-disk SQLite database the engine will then open.
fn seed_db() -> tempfile::NamedTempFile {
    let f = tempfile::Builder::new()
        .suffix(".sqlite")
        .tempfile()
        .unwrap();
    let conn = rusqlite::Connection::open(f.path()).unwrap();
    conn.execute_batch(
        "
        CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT, is_employee INTEGER);
        INSERT INTO users (id, email, is_employee) VALUES
            (1, 'alice@x.com', 1),
            (2, 'bob@x.com', 0),
            (3, 'carol@x.com', 1);
        ",
    )
    .unwrap();
    drop(conn);
    f
}

#[tokio::test]
async fn sqlite_count_and_filter_pushdown() {
    let f = seed_db();
    let (ctx, catalog) = build_ctx().await;

    let def = SourceDef {
        name: "oltp".into(),
        kind: SourceKind::Sqlite,
        description: None,
        config: json!({"path": f.path().to_string_lossy()}),
        cache: CachePolicy::None,
        safety: None,
        tables: Vec::new(),
        raw_table: false,
        raw_table_safety: None,
    };
    let report = register_sqlite_source(&def, &ctx, catalog.as_ref())
        .await
        .unwrap();
    assert_eq!(
        report.tables.iter().filter(|t| t.name == "users").count(),
        1
    );

    let df = ctx
        .sql("SELECT COUNT(*) AS n FROM oltp.users")
        .await
        .unwrap();
    let batches = df.collect().await.unwrap();
    let arr = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .unwrap();
    assert_eq!(arr.value(0), 3);

    let df = ctx
        .sql("SELECT email FROM oltp.users WHERE email = 'alice@x.com'")
        .await
        .unwrap();
    let batches = df.collect().await.unwrap();
    assert_eq!(batches[0].num_rows(), 1);
    let arr = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(arr.value(0), "alice@x.com");
}

#[tokio::test]
async fn sqlite_explicit_table_with_query() {
    let f = seed_db();
    let (ctx, catalog) = build_ctx().await;

    let def = SourceDef {
        name: "oltp".into(),
        kind: SourceKind::Sqlite,
        description: None,
        config: json!({"path": f.path().to_string_lossy()}),
        cache: CachePolicy::None,
        safety: None,
        tables: vec![pawrly_core::TableDef {
            name: "employees".into(),
            description: Some("Employees only".into()),
            config: json!({"query": "SELECT id, email FROM users WHERE is_employee = 1"}),
            cache: None,
            safety: None,
        }],
        raw_table: false,
        raw_table_safety: None,
    };
    register_sqlite_source(&def, &ctx, catalog.as_ref())
        .await
        .unwrap();

    let df = ctx
        .sql("SELECT count(*) AS n FROM oltp.employees")
        .await
        .unwrap();
    let batches = df.collect().await.unwrap();
    let arr = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .unwrap();
    assert_eq!(arr.value(0), 2);
}
