//! Acceptance: the TPC-H enrichment pattern — an HTTP table-valued function
//! joined with SQLite dimension tables (`nation`, `region`) by country name, the
//! same federated shape `examples/tpch/pawrly.yaml` uses (only there the dims are
//! the real generated SF1 data and the function hits a live API). Here a tiny
//! in-test SQLite + a wiremock responder keep it hermetic.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use pawrly_config::Config;
use pawrly_core::{EngineService, EngineServiceExt};
use pawrly_engine::{LocalEngine, LocalEngineConfig};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg_yaml(yaml: &str) -> Config {
    let secrets = pawrly_secrets::StaticStore::new();
    pawrly_config::load_str(yaml, &secrets).expect("config parse")
}

fn strings(batches: &[arrow_array::RecordBatch], name: &str) -> Vec<String> {
    use arrow_array::Array;
    let mut out = Vec::new();
    for b in batches {
        let idx = b.schema().index_of(name).unwrap();
        let arr = b
            .column(idx)
            .as_any()
            .downcast_ref::<arrow_array::StringArray>()
            .unwrap();
        for i in 0..arr.len() {
            out.push(arr.value(i).to_string());
        }
    }
    out
}

fn i64s(batches: &[arrow_array::RecordBatch], name: &str) -> Vec<i64> {
    use arrow_array::Array;
    let mut out = Vec::new();
    for b in batches {
        let idx = b.schema().index_of(name).unwrap();
        let a = b
            .column(idx)
            .as_any()
            .downcast_ref::<arrow_array::Int64Array>()
            .unwrap();
        for i in 0..a.len() {
            out.push(a.value(i));
        }
    }
    out
}

/// Build a tiny TPC-H `ref.sqlite` (nation + region) and an engine wiring it to
/// a wiremock `world` HTTP source that carries an attached `enrich` function.
/// Returns (engine, tempdir, server) — the latter two must outlive the engine.
async fn setup() -> (Arc<dyn EngineService>, TempDir, MockServer) {
    let dir = TempDir::new().unwrap();
    let sqlite = dir.path().join("ref.sqlite");
    let conn = rusqlite::Connection::open(&sqlite).unwrap();
    conn.execute_batch(
        "CREATE TABLE region (r_regionkey INTEGER, r_name TEXT);
         INSERT INTO region VALUES (1,'AMERICA'),(2,'ASIA'),(3,'EUROPE');
         CREATE TABLE nation (n_nationkey INTEGER, n_name TEXT, n_regionkey INTEGER);
         INSERT INTO nation VALUES
           (0,'BRAZIL',1),(1,'FRANCE',3),(2,'GERMANY',3),(3,'JAPAN',2);",
    )
    .unwrap();
    drop(conn);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/countries"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                { "name": "FRANCE",  "currency": "EUR", "iso3": "FRA" },
                { "name": "GERMANY", "currency": "EUR", "iso3": "DEU" },
                { "name": "BRAZIL",  "currency": "BRL", "iso3": "BRA" },
                { "name": "JAPAN",   "currency": "JPY", "iso3": "JPN" }
            ]
        })))
        .mount(&server)
        .await;

    let yaml = format!(
        r#"version: 1
sources:
  - name: ref
    kind: sqlite
    config:
      path: "{sqlite}"
  - name: world
    kind: http
    config:
      base_url: "{uri}"
    functions:
      - name: enrich
        endpoint: /countries
        args:
          - {{ name: scope, type: varchar, required: true }}
        response:
          path: $.data
        returns:
          - {{ name: country,  type: varchar, source: $.name }}
          - {{ name: currency, type: varchar, source: $.currency }}
          - {{ name: iso3,     type: varchar, source: $.iso3 }}
"#,
        sqlite = sqlite.display(),
        uri = server.uri(),
    );

    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&yaml),
        workspace_dir: dir.path().to_path_buf(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    (Arc::new(engine), dir, server)
}

/// 3-way federated join: HTTP function ⋈ SQLite `nation` ⋈ SQLite `region`,
/// joined by country name (the `world.currency`-vs-`ref.nation` pattern, as a
/// function).
#[tokio::test]
async fn enrich_function_joins_sqlite_nation_and_region() {
    let (svc, _dir, _server) = setup().await;

    let sql = "
        SELECT n.n_name AS nation,
               r.r_name AS region,
               e.currency AS currency,
               e.iso3 AS iso3
        FROM world.enrich('all') e
        JOIN ref.nation n  ON n.n_name = e.country
        JOIN ref.region r  ON r.r_regionkey = n.n_regionkey
        ORDER BY n.n_name";
    let b = svc.query_collect(sql).await.expect("query");

    assert_eq!(
        strings(&b, "nation"),
        vec!["BRAZIL", "FRANCE", "GERMANY", "JAPAN"]
    );
    assert_eq!(
        strings(&b, "region"),
        vec!["AMERICA", "EUROPE", "EUROPE", "ASIA"]
    );
    assert_eq!(strings(&b, "currency"), vec!["BRL", "EUR", "EUR", "JPY"]);
    assert_eq!(strings(&b, "iso3"), vec!["BRA", "FRA", "DEU", "JPN"]);
}

/// Aggregate over the federated join: count nations per currency, grouped by a
/// column that came from the HTTP function.
#[tokio::test]
async fn currency_rollup_over_function_and_nation() {
    let (svc, _dir, _server) = setup().await;

    let sql = "
        WITH enriched AS (
            SELECT n.n_name, n.n_regionkey, e.currency
            FROM world.enrich('all') e
            JOIN ref.nation n ON n.n_name = e.country
        )
        SELECT en.currency AS currency,
               r.r_name AS region,
               COUNT(*) AS nations
        FROM enriched en
        JOIN ref.region r ON r.r_regionkey = en.n_regionkey
        GROUP BY en.currency, r.r_name
        HAVING COUNT(*) >= 1
        ORDER BY nations DESC, currency";
    let b = svc.query_collect(sql).await.expect("query");

    // EUR spans France+Germany (both EUROPE) → 2; BRL/JPY → 1 each.
    assert_eq!(strings(&b, "currency"), vec!["EUR", "BRL", "JPY"]);
    assert_eq!(strings(&b, "region"), vec!["EUROPE", "AMERICA", "ASIA"]);
    assert_eq!(i64s(&b, "nations"), vec![2, 1, 1]);
}
