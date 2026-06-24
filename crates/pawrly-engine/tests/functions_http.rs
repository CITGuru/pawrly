//! Acceptance: a declared `http` table-valued function end-to-end through the
//! engine, against a wiremock server — exercises rewrite → UDTF → bind_args →
//! HttpFunctionExecutor → fetch pipeline → WHERE-on-top.

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
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg_yaml(yaml: &str) -> Config {
    let secrets = pawrly_secrets::StaticStore::new();
    pawrly_config::load_str(yaml, &secrets).expect("config parse")
}

fn string_col(batch: &arrow_array::RecordBatch, name: &str) -> Vec<String> {
    use arrow_array::Array;
    let idx = batch.schema().index_of(name).unwrap();
    let arr = batch
        .column(idx)
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    (0..arr.len()).map(|i| arr.value(i).to_string()).collect()
}

#[tokio::test]
async fn declared_http_function_end_to_end() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .and(query_param("q", "is:open"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [
                { "number": 1, "title": "first",  "user": { "login": "alice" } },
                { "number": 2, "title": "second", "user": { "login": "bob" } },
            ]
        })))
        .mount(&server)
        .await;

    let yaml = format!(
        r#"version: 1
functions:
  - name: search_issues
    namespace: gh
    kind: http
    config:
      base_url: "{}"
    endpoint: /search/issues
    args:
      - {{ name: q,     type: varchar, required: true }}
      - {{ name: limit, type: int,     default: "50" }}
    response:
      path: $.items
    returns:
      - {{ name: number,     type: bigint }}
      - {{ name: title,      type: varchar }}
      - {{ name: user_login, type: varchar, source: $.user.login }}
      - {{ name: q,          type: varchar, source: arg }}
"#,
        server.uri()
    );

    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&yaml),
        workspace_dir: std::env::temp_dir(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    // Full call: the `q` arg is sent as `?q=is:open` (asserted by the mock
    // matcher), rows come from `$.items`, and `source: arg` echoes `q`.
    let batches = svc
        .query_collect(
            "SELECT title, user_login, q FROM gh.search_issues('is:open', 5) ORDER BY title",
        )
        .await
        .expect("query");
    let titles: Vec<String> = batches
        .iter()
        .flat_map(|b| string_col(b, "title"))
        .collect();
    assert_eq!(titles, vec!["first".to_string(), "second".to_string()]);
    let logins: Vec<String> = batches
        .iter()
        .flat_map(|b| string_col(b, "user_login"))
        .collect();
    assert_eq!(logins, vec!["alice".to_string(), "bob".to_string()]);
    // `source: arg` injects the bound call argument as a constant column.
    let qs: Vec<String> = batches.iter().flat_map(|b| string_col(b, "q")).collect();
    assert_eq!(qs, vec!["is:open".to_string(), "is:open".to_string()]);

    // A WHERE filter applies on top of the function result.
    let batches = svc
        .query_collect("SELECT title FROM gh.search_issues('is:open') WHERE title = 'second'")
        .await
        .expect("query");
    let titles: Vec<String> = batches
        .iter()
        .flat_map(|b| string_col(b, "title"))
        .collect();
    assert_eq!(titles, vec!["second".to_string()]);
}

#[tokio::test]
async fn missing_required_arg_is_a_clear_error() {
    let server = MockServer::start().await;
    let yaml = format!(
        r#"version: 1
functions:
  - name: search_issues
    namespace: gh
    kind: http
    config:
      base_url: "{}"
    endpoint: /search/issues
    args:
      - {{ name: q, type: varchar, required: true }}
    response:
      path: $.items
    returns:
      - {{ name: title, type: varchar }}
"#,
        server.uri()
    );

    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&yaml),
        workspace_dir: std::env::temp_dir(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    let err = svc
        .query_collect("SELECT title FROM gh.search_issues()")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("requires argument `q`"), "{err}");
}

/// An http function **attached** to a source inherits the source's namespace,
/// connection, and auth (and shares its live handle). The mock requires the
/// source's `Authorization` header, so the call only succeeds if the attached
/// function reaches the API through the parent source's configured auth.
#[tokio::test]
async fn attached_http_function_inherits_source_auth() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .and(wiremock::matchers::header("authorization", "Bearer T0K3N"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{ "title": "ok" }]
        })))
        .mount(&server)
        .await;

    let yaml = format!(
        r#"version: 1
sources:
  - name: gh
    kind: http
    config:
      base_url: "{}"
      auth:
        type: header
        headers:
          - {{ name: Authorization, bearer: "T0K3N" }}
    functions:
      - name: search_issues
        endpoint: /search/issues
        args:
          - {{ name: q, type: varchar, required: true }}
        response:
          path: $.items
        returns:
          - {{ name: title, type: varchar }}
"#,
        server.uri()
    );

    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml(&yaml),
        workspace_dir: std::env::temp_dir(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    let batches = svc
        .query_collect("SELECT title FROM gh.search_issues('is:open')")
        .await
        .expect("query");
    let titles: Vec<String> = batches
        .iter()
        .flat_map(|b| string_col(b, "title"))
        .collect();
    assert_eq!(titles, vec!["ok".to_string()]);
}

/// The builtin `http.get(url, path)`: the `path` arg is templated into the
/// response JSONPath per call (and not sent to the API); each matched element
/// is returned as JSON in the `body` column.
#[tokio::test]
async fn builtin_http_get_end_to_end() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{ "id": 1, "name": "a" }, { "id": 2, "name": "b" }]
        })))
        .mount(&server)
        .await;

    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg_yaml("version: 1"),
        workspace_dir: std::env::temp_dir(),
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    let svc: Arc<dyn EngineService> = Arc::new(engine);

    let sql = format!(
        "SELECT body FROM http.get('{}/items', '$.items')",
        server.uri()
    );
    let batches = svc.query_collect(&sql).await.expect("query");
    let bodies: Vec<String> = batches.iter().flat_map(|b| string_col(b, "body")).collect();
    assert_eq!(bodies.len(), 2);
    // Each row is the matched element serialized as JSON.
    assert!(bodies[0].contains("\"name\":\"a\""), "{:?}", bodies[0]);
    assert!(bodies[1].contains("\"name\":\"b\""), "{:?}", bodies[1]);
}
