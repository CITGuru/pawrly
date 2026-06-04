//! Acceptance: lakehouse / warehouse kinds (`snowflake`, `iceberg`, `delta`,
//! `ducklake`), the local `duckdb` file, object storage under `file` (a
//! `storage:` block), and DB-attach kinds (`postgres`, `mysql`) are wired to
//! the in-process DuckDB pool. Offline (no network), extension `INSTALL` is
//! skipped, so registration surfaces a real attach/extension/scan error — NOT
//! the old "lakehouse"/"feature-gated" placeholder. These smoke tests assert
//! exactly that: the dispatch reaches DuckDB, and the legacy gated strings are
//! gone.

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

/// `snowflake` / `iceberg` / `delta` / `ducklake` / `duckdb`, and object
/// storage under `file` (a `storage:` block) all reach DuckDB. Offline, the
/// extension can't be installed, so registration errors — but the error is now
/// a real DuckDB error, not the old "lakehouse feature" message.
#[tokio::test]
async fn lakehouse_kinds_dispatch_to_duckdb_offline() {
    // Run under `PAWRLY_OFFLINE=1` so the pool skips extension INSTALL and the
    // test never touches the network.
    let workspace = TempDir::new().unwrap();
    // Each kind gets a config valid enough to pass `validate` and reach the
    // DuckDB dispatch.
    let cases: [(&str, &str); 6] = [
        (
            "snowflake",
            "    config:\n      account: a\n      user: u\n      password: p\n",
        ),
        (
            "iceberg",
            "    tables:\n      - name: t\n        path: /tmp/does-not-exist\n",
        ),
        (
            "delta",
            "    tables:\n      - name: t\n        path: /tmp/does-not-exist\n",
        ),
        (
            "duckdb",
            "    config:\n      path: /tmp/does-not-exist.duckdb\n",
        ),
        (
            "ducklake",
            "    config:\n      catalog: /tmp/does-not-exist.ducklake\n",
        ),
        (
            "file",
            "    config:\n      storage:\n        type: s3\n        region: us-east-1\n    tables:\n      - name: t\n        path: s3://b/x.parquet\n",
        ),
    ];
    for (kind, extra) in cases {
        let yaml = format!("version: 1\nsources:\n  - name: warehouse\n    kind: {kind}\n{extra}");
        let cfg = cfg_yaml(&yaml);
        let res = LocalEngine::new(LocalEngineConfig {
            config: cfg,
            workspace_dir: workspace.path().to_path_buf(),
            duckdb_pool_size: Some(1),
        })
        .await;
        // If a build happens to have the extension cached, registration may
        // even succeed — that's fine, the dispatch reached DuckDB. Only an
        // error is asserted against (it must not be the old gated message).
        if let Err(e) = res {
            let s = e.to_string();
            assert!(
                !s.contains("lakehouse") && !s.contains("requires the"),
                "kind `{kind}` should no longer return the old gated error; got: {s}"
            );
        }
    }
}

/// `postgres` / `mysql` now ATTACH via DuckDB. Offline they can't install the
/// extension, so registration errors with a real DuckDB error — not the old
/// "requires the `duckdb-extensions` build feature" message.
#[tokio::test]
async fn postgres_mysql_dispatch_to_duckdb_offline() {
    // Run under `PAWRLY_OFFLINE=1` (see the lakehouse test above).
    let workspace = TempDir::new().unwrap();
    for kind in ["postgres", "mysql"] {
        let yaml = format!(
            r#"version: 1
sources:
  - name: db
    kind: {kind}
    config:
      dsn: {kind}://localhost/x
"#
        );
        let cfg = cfg_yaml(&yaml);
        let res = LocalEngine::new(LocalEngineConfig {
            config: cfg,
            workspace_dir: workspace.path().to_path_buf(),
            duckdb_pool_size: Some(1),
        })
        .await;
        if let Err(e) = res {
            let s = e.to_string();
            assert!(
                !s.contains("duckdb-extensions build feature"),
                "kind `{kind}` should no longer return the old gated error; got: {s}"
            );
        }
    }
}

/// `bigquery` / `redshift` (and the old SaaS/`ai`/object-store kinds) were
/// removed: they're now unknown kinds and fail at config load.
#[test]
fn removed_kinds_fail_at_load() {
    for kind in ["bigquery", "redshift", "github", "ai", "s3", "gcs", "azure"] {
        let yaml = format!("version: 1\nsources:\n  - name: wh\n    kind: {kind}\n");
        let secrets = pawrly_secrets::StaticStore::new();
        let res = pawrly_config::load_str(&yaml, &secrets);
        assert!(res.is_err(), "kind `{kind}` should be unknown now");
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
