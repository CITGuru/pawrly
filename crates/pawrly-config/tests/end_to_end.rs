//! Acceptance: load the kitchen-sink example, with all its secrets
//! mocked, and round-trip a query through `MockEngine`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::path::PathBuf;
use std::sync::Arc;

use pawrly_config::{load, validate};
use pawrly_core::test_support::MockEngine;
use pawrly_core::{
    ColumnSpec, EngineService, EngineServiceExt, QueryRequest, SourceKind, TableName,
};
use pawrly_secrets::StaticStore;

fn workspace_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn mock_secrets() -> StaticStore {
    let s = StaticStore::new();
    s.insert("GITHUB_TOKEN", "ghp_test_token_value");
    s.insert("LINEAR_API_KEY", "lin_test_key");
    s.insert("PG_DSN", "postgresql://test:test@localhost/test");
    s.insert("SNOWFLAKE_USER", "svc_pawrly");
    s.insert("SNOWFLAKE_PASSWORD", "p4ssw0rd!");
    s.insert("AWS_KEY_ID", "AKIAEXAMPLE");
    s.insert("AWS_SECRET", "secret-example");
    s
}

#[test]
fn loads_kitchen_sink_example() {
    let path = workspace_dir().join("examples").join("pawrly.yaml");
    let secrets = mock_secrets();
    let cfg = load(&path, &secrets).unwrap_or_else(|e| panic!("failed to load {path:?}: {e}"));

    assert_eq!(cfg.version, 1);
    assert_eq!(cfg.name, "kitchen-sink");

    // Every named source from the kitchen-sink example should be present.
    let names: Vec<&str> = cfg.sources.iter().map(|s| s.name.as_str()).collect();
    for expected in [
        "data",
        "lake",
        "gh",
        "linear",
        "oltp",
        "warehouse",
        "local_db",
        "dl",
    ] {
        assert!(
            names.contains(&expected),
            "missing source `{expected}` in {names:?}"
        );
    }

    // Validation should produce no errors.
    let errs = validate(&cfg);
    assert!(errs.is_empty(), "validation errors: {errs}");

    // Secrets must have been resolved: `gh.config.token` is the resolved value, not the ref.
    let gh = cfg.sources.iter().find(|s| s.name == "gh").unwrap();
    let token = gh.config["token"].as_str().unwrap();
    assert_eq!(token, "ghp_test_token_value");

    // raw_table flag is preserved.
    assert!(gh.raw_table, "gh should have raw_table: true");

    // Agent-facing metadata parses: source + table wiki, and examples.
    assert!(gh.wiki.is_some(), "gh should have a wiki");
    assert!(gh.tables[0].wiki.is_some(), "gh.pulls should have a wiki");
    assert_eq!(gh.examples.len(), 1);
}

#[tokio::test]
async fn mock_engine_round_trip_through_trait() {
    // Build a fake engine that returns a canned batch when asked.
    let engine = Arc::new(MockEngine::new());
    engine.add_source("data", SourceKind::File);
    engine.add_table(
        TableName::new("data", "orders"),
        SourceKind::File,
        vec![
            ColumnSpec {
                name: "id".into(),
                data_type: "Int64".into(),
                nullable: false,
                description: None,
                is_filter_pushable: false,
                is_required_filter: false,
            },
            ColumnSpec {
                name: "customer".into(),
                data_type: "Utf8".into(),
                nullable: true,
                description: None,
                is_filter_pushable: false,
                is_required_filter: false,
            },
        ],
    );
    engine.canned("FROM data.orders", vec![MockEngine::one_row(42, "acme")]);

    // Use the engine through the trait. This is the key test: every frontend
    // (CLI, MCP, library) does this same dance.
    let svc: Arc<dyn EngineService> = engine.clone();

    // 1. discovery
    let sources = svc.list_sources().await.unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].name, "data");

    let tables = svc.list_tables(None).await.unwrap();
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name.to_string(), "data.orders");

    let desc = svc
        .describe_table(&TableName::new("data", "orders"))
        .await
        .unwrap();
    assert_eq!(desc.columns.len(), 2);

    // 2. query — uses the EngineServiceExt::query_collect convenience.
    let batches = svc
        .query_collect("SELECT * FROM data.orders WHERE id = 42")
        .await
        .unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 1);
    assert_eq!(batch.num_columns(), 2);

    // 3. health works
    let h = svc.health().await.unwrap();
    assert!(h.ok);
    assert_eq!(h.sources_ok, 1);

    // 4. the engine recorded the SQL we issued
    let seen = engine.queries_seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].contains("FROM data.orders"));
}

#[test]
fn rejects_unknown_kind() {
    let yaml = r#"
version: 1
sources:
  - name: x
    kind: nonsense
"#;
    let secrets = StaticStore::new();
    let res = pawrly_config::load_str(yaml, &secrets);
    assert!(res.is_err());
}

#[test]
fn engine_request_round_trips() {
    let req = QueryRequest::sql("SELECT 1");
    assert_eq!(req.sql, "SELECT 1");
}

#[test]
fn wiki_maps_through_to_engine_defs() {
    let yaml = r#"version: 1
sources:
  - name: gh
    kind: http
    wiki: "All endpoints need owner/repo filters."
    examples:
      - SELECT * FROM gh.pulls LIMIT 1
    config:
      base_url: https://api.github.com
    tables:
      - name: pulls
        wiki: "state defaults to open."
        endpoint: /repos/{owner}/{repo}/pulls
        response:
          array: true
"#;
    let secrets = StaticStore::new();
    let cfg = pawrly_config::load_str(yaml, &secrets).expect("parse");
    let defs = cfg.into_engine_sources();
    assert_eq!(
        defs[0].wiki.as_deref(),
        Some("All endpoints need owner/repo filters.")
    );
    assert_eq!(
        defs[0].tables[0].wiki.as_deref(),
        Some("state defaults to open.")
    );
    assert_eq!(defs[0].examples, vec!["SELECT * FROM gh.pulls LIMIT 1"]);
}
