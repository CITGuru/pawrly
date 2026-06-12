//! Acceptance: register a file source over fixture parquet/csv/json
//! and run real SQL queries against it.

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
    pawrly_config::load_str(yaml, &secrets).expect("config parse")
}

#[tokio::test]
async fn parquet_count_via_local_engine() {
    let dir = fixtures_dir();
    let yaml = format!(
        r#"version: 1
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
        dir.display(),
        dir.join("orders.parquet").display(),
    );

    let cfg = cfg_yaml(&yaml);
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: dir.clone(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");

    let svc: Arc<dyn EngineService> = Arc::new(engine);

    let batches = svc
        .query_collect("SELECT COUNT(*) AS n FROM data.orders")
        .await
        .expect("query");
    assert_eq!(batches.len(), 1);
    let arr = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .expect("int64 result");
    assert_eq!(arr.value(0), 5);

    // Catalog introspection works.
    let tables = svc.list_tables(None).await.expect("list_tables");
    assert!(tables.iter().any(|t| t.name.to_string() == "data.orders"));

    let desc = svc
        .describe_table(&pawrly_core::TableName::new("data", "orders"))
        .await
        .expect("describe");
    let names: Vec<_> = desc.columns.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"id"));
    assert!(names.contains(&"customer"));
    assert!(names.contains(&"amount_cents"));
}

#[tokio::test]
async fn csv_query_via_local_engine() {
    let dir = fixtures_dir();
    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{}"
    tables:
      - name: customers
        path: "{}"
        format: csv
"#,
        dir.display(),
        dir.join("customers.csv").display(),
    );

    let cfg = cfg_yaml(&yaml);
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: dir.clone(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");

    let svc: Arc<dyn EngineService> = Arc::new(engine);
    let batches = svc
        .query_collect("SELECT plan FROM data.customers WHERE id = 1")
        .await
        .expect("csv query");
    assert_eq!(batches.len(), 1);
    let arr = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .expect("string result");
    assert_eq!(arr.value(0), "enterprise");
}

#[tokio::test]
async fn json_query_via_local_engine() {
    let dir = fixtures_dir();
    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{}"
    tables:
      - name: events
        path: "{}"
        format: json
"#,
        dir.display(),
        dir.join("events.json").display(),
    );

    let cfg = cfg_yaml(&yaml);
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: dir.clone(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");

    let svc: Arc<dyn EngineService> = Arc::new(engine);
    let batches = svc
        .query_collect("SELECT COUNT(*) AS n FROM data.events WHERE event = 'login'")
        .await
        .expect("json query");
    let arr = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .expect("int64");
    assert_eq!(arr.value(0), 2);
}

#[tokio::test]
async fn refresh_catalog_re_enumerates_glob() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::write(dir.join("a.csv"), "id,n\n1,alpha\n").unwrap();
    std::fs::write(dir.join("b.csv"), "id,n\n2,beta\n").unwrap();

    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{}"
"#,
        dir.join("*.csv").display(),
    );
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&yaml),
        workspace_dir: dir.clone(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    assert_eq!(svc.list_tables(None).await.unwrap().len(), 2);

    // A new file added after build is picked up only after a refresh.
    std::fs::write(dir.join("c.csv"), "id,n\n3,gamma\n").unwrap();
    assert_eq!(svc.list_tables(None).await.unwrap().len(), 2);

    let outcome = svc.refresh_catalog(None).await.expect("refresh");
    assert_eq!(outcome.sources_refreshed, 1);
    assert_eq!(outcome.tables_discovered, 3);
    let tables = svc.list_tables(None).await.unwrap();
    assert_eq!(tables.len(), 3);
    assert!(tables.iter().any(|t| t.name.to_string() == "data.c"));

    // A removed file drops out on the next refresh of just that source.
    std::fs::remove_file(dir.join("b.csv")).unwrap();
    let outcome = svc
        .refresh_catalog(Some("data"))
        .await
        .expect("refresh one");
    assert_eq!(outcome.tables_discovered, 2);
    assert!(
        !svc.list_tables(None)
            .await
            .unwrap()
            .iter()
            .any(|t| t.name.to_string() == "data.b")
    );
}

#[tokio::test]
async fn reload_config_diffs_sources() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::write(dir.join("a.csv"), "id,n\n1,alpha\n").unwrap();
    std::fs::write(dir.join("b.csv"), "id,n\n2,beta\n").unwrap();
    let cfg_path = dir.join("pawrly.yaml");

    let one_only = format!(
        "version: 1\nsources:\n  - name: one\n    kind: file\n    config:\n      path: \"{}\"\n",
        dir.join("a.csv").display(),
    );
    std::fs::write(&cfg_path, &one_only).unwrap();

    let engine = LocalEngine::from_config_file(&cfg_path)
        .await
        .expect("engine");
    let svc: Arc<dyn EngineService> = Arc::new(engine);
    assert_eq!(svc.list_sources().await.unwrap().len(), 1);

    // Add a second source.
    std::fs::write(
        &cfg_path,
        format!(
            "version: 1\nsources:\n  - name: one\n    kind: file\n    config:\n      path: \"{}\"\n  - name: two\n    kind: file\n    config:\n      path: \"{}\"\n",
            dir.join("a.csv").display(),
            dir.join("b.csv").display(),
        ),
    )
    .unwrap();
    let report = svc.reload_config().await.expect("reload add");
    assert_eq!(
        (
            report.sources_added,
            report.sources_changed,
            report.sources_removed
        ),
        (1, 0, 0)
    );
    assert_eq!(svc.list_sources().await.unwrap().len(), 2);

    // Change "one" (new description), leave "two" untouched.
    std::fs::write(
        &cfg_path,
        format!(
            "version: 1\nsources:\n  - name: one\n    kind: file\n    description: edited\n    config:\n      path: \"{}\"\n  - name: two\n    kind: file\n    config:\n      path: \"{}\"\n",
            dir.join("a.csv").display(),
            dir.join("b.csv").display(),
        ),
    )
    .unwrap();
    let report = svc.reload_config().await.expect("reload change");
    assert_eq!(
        (
            report.sources_added,
            report.sources_changed,
            report.sources_removed
        ),
        (0, 1, 0)
    );

    // Drop "one".
    std::fs::write(
        &cfg_path,
        format!(
            "version: 1\nsources:\n  - name: two\n    kind: file\n    config:\n      path: \"{}\"\n",
            dir.join("b.csv").display(),
        ),
    )
    .unwrap();
    let report = svc.reload_config().await.expect("reload remove");
    assert_eq!(
        (
            report.sources_added,
            report.sources_changed,
            report.sources_removed
        ),
        (0, 0, 1)
    );
    let sources = svc.list_sources().await.unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].name, "two");
}

#[tokio::test]
async fn describe_table_surfaces_wiki_and_description() {
    let dir = fixtures_dir();
    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    description: fixture data
    wiki: "All timestamps are UTC."
    examples:
      - SELECT * FROM data.orders LIMIT 1
      - SELECT id FROM data.customers LIMIT 1
    config:
      path: "{}"
    tables:
      - name: orders
        description: order rows
        wiki: "Amounts are integer cents; divide by 100."
        path: "{}"
        format: parquet
      - name: customers
        path: "{}"
        format: csv
"#,
        dir.display(),
        dir.join("orders.parquet").display(),
        dir.join("customers.csv").display(),
    );

    let cfg = cfg_yaml(&yaml);
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: dir.clone(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    // Table with its own wiki: source notes first, then table notes.
    let desc = svc
        .describe_table(&pawrly_core::TableName::new("data", "orders"))
        .await
        .expect("describe orders");
    assert_eq!(
        desc.wiki.as_deref(),
        Some("All timestamps are UTC.\n\nAmounts are integer cents; divide by 100.")
    );
    assert_eq!(desc.table.description.as_deref(), Some("order rows"));
    // Only the examples mentioning this table are surfaced.
    assert_eq!(desc.examples, vec!["SELECT * FROM data.orders LIMIT 1"]);

    // Table without its own wiki falls back to the source-level notes.
    let desc = svc
        .describe_table(&pawrly_core::TableName::new("data", "customers"))
        .await
        .expect("describe customers");
    assert_eq!(desc.wiki.as_deref(), Some("All timestamps are UTC."));
    assert_eq!(desc.examples, vec!["SELECT id FROM data.customers LIMIT 1"]);
}
