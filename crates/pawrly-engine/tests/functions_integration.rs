//! Acceptance: table-valued functions used in complex queries — joined with
//! real tables (parquet/csv/json fixtures), joined with other functions, and
//! the same function called with different literal args — exercising CTEs,
//! aggregation, window functions, subqueries, and `WHERE`/`HAVING` on top.
//!
//! Data model:
//!   data.orders    (id, customer<slug>, amount_cents) — acme, ben, ben, delta, echo
//!   data.customers (id 1-3, name, plan)
//!   data.events    (id 1-4, event, ts)
//!   api.accounts(tier)  → {slug, id, mrr_cents, region}  (a wiremock HTTP fn)
//!   api.usage(metric)   → {id, n}                        (a wiremock HTTP fn)
//! The `accounts` function bridges `orders` (keyed by slug) and `customers`
//! (keyed by id), which share no key directly.

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
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
        let col = b.column(idx);
        if let Some(a) = col.as_any().downcast_ref::<arrow_array::Int64Array>() {
            for i in 0..a.len() {
                out.push(a.value(i));
            }
        } else if let Some(a) = col.as_any().downcast_ref::<arrow_array::UInt64Array>() {
            for i in 0..a.len() {
                out.push(a.value(i) as i64);
            }
        } else {
            panic!("column {name} is not an integer array");
        }
    }
    out
}

async fn mock_api() -> MockServer {
    let server = MockServer::start().await;

    // accounts(tier) → all three accounts, regardless of tier (sent as ?tier=).
    Mock::given(method("GET"))
        .and(path("/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accounts": [
                { "slug": "acme",  "id": 1, "mrr_cents": 50000, "region": "us" },
                { "slug": "ben",   "id": 2, "mrr_cents": 12000, "region": "eu" },
                { "slug": "delta", "id": 3, "mrr_cents": 3000,  "region": "us" }
            ]
        })))
        .mount(&server)
        .await;

    // usage(metric) → different rows per metric (asserts the arg drives the API).
    Mock::given(method("GET"))
        .and(path("/usage"))
        .and(query_param("metric", "logins"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "usage": [{ "id": 1, "n": 12 }, { "id": 2, "n": 7 }, { "id": 3, "n": 3 }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/usage"))
        .and(query_param("metric", "signups"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "usage": [{ "id": 1, "n": 2 }, { "id": 2, "n": 5 }, { "id": 3, "n": 1 }]
        })))
        .mount(&server)
        .await;

    server
}

async fn engine(server: &MockServer) -> Arc<dyn EngineService> {
    let dir = fixtures_dir();
    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{dir}"
    tables:
      - {{ name: orders,    path: "{dir}/orders.parquet", format: parquet }}
      - {{ name: customers, path: "{dir}/customers.csv",  format: csv }}
      - {{ name: events,    path: "{dir}/events.json",    format: json }}
  - name: api
    kind: http
    config:
      base_url: "{uri}"
    functions:
      - name: accounts
        endpoint: /accounts
        args:
          - {{ name: tier, type: varchar, required: true }}
        response:
          path: $.accounts
        returns:
          - {{ name: slug,      type: varchar }}
          - {{ name: id,        type: bigint }}
          - {{ name: mrr_cents, type: bigint }}
          - {{ name: region,    type: varchar }}
      - name: usage
        endpoint: /usage
        args:
          - {{ name: metric, type: varchar, required: true }}
        response:
          path: $.usage
        returns:
          - {{ name: id, type: bigint }}
          - {{ name: n,  type: bigint }}
"#,
        dir = dir.display(),
        uri = server.uri(),
    );

    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&yaml),
        workspace_dir: dir,
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    Arc::new(engine)
}

/// Function bridges two key-incompatible tables, with aggregation, a window
/// rank, HAVING, and ORDER BY.
#[tokio::test]
async fn function_bridges_two_tables_with_window_and_having() {
    let server = mock_api().await;
    let svc = engine(&server).await;

    let sql = "
        SELECT c.name AS name,
               a.region AS region,
               SUM(o.amount_cents) AS total,
               RANK() OVER (ORDER BY SUM(o.amount_cents) DESC) AS rk
        FROM api.accounts('all') a
        JOIN data.orders o     ON o.customer = a.slug
        JOIN data.customers c  ON c.id = a.id
        GROUP BY c.name, a.region
        HAVING SUM(o.amount_cents) > 100
        ORDER BY total DESC";
    let b = svc.query_collect(sql).await.expect("query");

    assert_eq!(strings(&b, "name"), vec!["Ben LLC", "Acme Corp"]);
    assert_eq!(strings(&b, "region"), vec!["eu", "us"]);
    assert_eq!(i64s(&b, "total"), vec![6700, 1000]); // ben 2500+4200, acme 1000; delta 50 cut by HAVING
    assert_eq!(i64s(&b, "rk"), vec![1, 2]);
}

/// The same function called with two different literal args, self-joined, then
/// joined with a table — via CTEs.
#[tokio::test]
async fn function_self_joined_on_different_args() {
    let server = mock_api().await;
    let svc = engine(&server).await;

    let sql = "
        WITH logins  AS (SELECT id, n FROM api.usage('logins')),
             signups AS (SELECT id, n FROM api.usage('signups'))
        SELECT c.name AS name,
               l.n AS logins,
               s.n AS signups,
               (l.n - s.n) AS net
        FROM logins l
        JOIN signups s         ON s.id = l.id
        JOIN data.customers c  ON c.id = l.id
        ORDER BY net DESC, name ASC";
    let b = svc.query_collect(sql).await.expect("query");

    assert_eq!(
        strings(&b, "name"),
        vec!["Acme Corp", "Ben LLC", "Delta Co"]
    );
    assert_eq!(i64s(&b, "logins"), vec![12, 7, 3]);
    assert_eq!(i64s(&b, "signups"), vec![2, 5, 1]);
    assert_eq!(i64s(&b, "net"), vec![10, 2, 2]);
}

/// Mega query: two functions + three tables + nested CTEs + LEFT JOINs +
/// COALESCE over aggregates, ordered by an HTTP-sourced metric.
#[tokio::test]
async fn two_functions_three_tables_nested_ctes() {
    let server = mock_api().await;
    let svc = engine(&server).await;

    let sql = "
        WITH acct AS (
            SELECT a.id, a.slug, a.mrr_cents, c.name, c.plan
            FROM api.accounts('all') a
            JOIN data.customers c ON c.id = a.id
        ),
        spend AS (
            SELECT customer AS slug, SUM(amount_cents) AS total
            FROM data.orders GROUP BY customer
        ),
        ev AS (
            SELECT id, COUNT(*) AS events FROM data.events GROUP BY id
        )
        SELECT acct.name AS name,
               acct.plan AS plan,
               COALESCE(spend.total, 0) AS order_total,
               COALESCE(ev.events, 0)   AS events,
               u.n AS logins
        FROM acct
        LEFT JOIN spend ON spend.slug = acct.slug
        LEFT JOIN ev    ON ev.id = acct.id
        JOIN api.usage('logins') u ON u.id = acct.id
        ORDER BY acct.mrr_cents DESC";
    let b = svc.query_collect(sql).await.expect("query");

    assert_eq!(
        strings(&b, "name"),
        vec!["Acme Corp", "Ben LLC", "Delta Co"]
    );
    assert_eq!(strings(&b, "plan"), vec!["enterprise", "team", "starter"]);
    assert_eq!(i64s(&b, "order_total"), vec![1000, 6700, 50]);
    assert_eq!(i64s(&b, "events"), vec![1, 1, 1]);
    assert_eq!(i64s(&b, "logins"), vec![12, 7, 3]);
}

/// A function filtered by a subquery over a table, grouped and aggregated.
#[tokio::test]
async fn function_filtered_by_table_subquery() {
    let server = mock_api().await;
    let svc = engine(&server).await;

    let sql = "
        SELECT region,
               COUNT(*) AS accounts,
               SUM(mrr_cents) AS mrr
        FROM api.accounts('all')
        WHERE id IN (SELECT id FROM data.customers WHERE plan <> 'starter')
        GROUP BY region
        ORDER BY region";
    let b = svc.query_collect(sql).await.expect("query");

    // starter (delta, id 3, region us) is excluded → us has only acme.
    assert_eq!(strings(&b, "region"), vec!["eu", "us"]);
    assert_eq!(i64s(&b, "accounts"), vec![1, 1]);
    assert_eq!(i64s(&b, "mrr"), vec![12000, 50000]);
}

/// The builtin `file.glob` joined with a declared HTTP function in one query.
#[tokio::test]
async fn builtin_glob_unioned_and_counted() {
    let server = mock_api().await;
    let svc = engine(&server).await;
    let dir = fixtures_dir();

    // file.glob over the fixtures dir, filtered + counted.
    let sql = format!(
        "SELECT count(*) AS n FROM file.glob('{}/*') WHERE file_name LIKE '%.csv' OR file_name LIKE '%.json'",
        dir.display()
    );
    let b = svc.query_collect(&sql).await.expect("query");
    assert_eq!(i64s(&b, "n"), vec![2]); // customers.csv + events.json

    // Cross-source: a function's row count alongside a builtin's, in one query.
    let sql = "
        SELECT
            (SELECT COUNT(*) FROM api.accounts('all'))           AS api_accounts,
            (SELECT COUNT(*) FROM api.usage('logins'))           AS api_usage,
            (SELECT COUNT(*) FROM data.customers)                AS customers";
    let b = svc.query_collect(sql).await.expect("query");
    assert_eq!(i64s(&b, "api_accounts"), vec![3]);
    assert_eq!(i64s(&b, "api_usage"), vec![3]);
    assert_eq!(i64s(&b, "customers"), vec![3]);
}
