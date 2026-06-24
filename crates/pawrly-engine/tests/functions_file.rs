//! Acceptance: table-valued functions end-to-end through the engine — the
//! builtin `file.glob` and a declared `file` function with an argument.

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

fn cfg_yaml(yaml: &str) -> Config {
    let secrets = pawrly_secrets::StaticStore::new();
    pawrly_config::load_str(yaml, &secrets).expect("config parse")
}

async fn engine_for(cfg: Config, workspace: PathBuf) -> Arc<dyn EngineService> {
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: workspace,
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    Arc::new(engine)
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pawrly_fn_file_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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
async fn builtin_file_glob_end_to_end() {
    let dir = tempdir("glob");
    std::fs::write(dir.join("b.csv"), b"x").unwrap();
    std::fs::write(dir.join("a.csv"), b"yy").unwrap();
    std::fs::write(dir.join("ignore.txt"), b"z").unwrap();

    let svc = engine_for(cfg_yaml("version: 1"), dir.clone()).await;

    let sql = format!(
        "SELECT file_name, size_bytes FROM file.glob('{}/*.csv') ORDER BY file_name",
        dir.display()
    );
    let batches = svc.query_collect(&sql).await.expect("query");
    let names: Vec<String> = batches
        .iter()
        .flat_map(|b| string_col(b, "file_name"))
        .collect();
    assert_eq!(names, vec!["a.csv".to_string(), "b.csv".to_string()]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn builtin_file_glob_zero_matches_is_empty() {
    let dir = tempdir("empty");
    let svc = engine_for(cfg_yaml("version: 1"), dir.clone()).await;
    let sql = format!("SELECT * FROM file.glob('{}/*.none')", dir.display());
    let batches = svc.query_collect(&sql).await.expect("query");
    let rows: usize = batches.iter().map(arrow_array::RecordBatch::num_rows).sum();
    assert_eq!(rows, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn declared_file_function_with_arg_placeholder() {
    let dir = tempdir("declared");
    std::fs::create_dir_all(dir.join("app")).unwrap();
    std::fs::write(dir.join("app").join("one.log"), b"a").unwrap();
    std::fs::write(dir.join("app").join("two.log"), b"b").unwrap();
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("db").join("other.log"), b"c").unwrap();

    let yaml = format!(
        r#"version: 1
functions:
  - name: logs
    namespace: ops
    kind: file
    path: "{}/{{service}}/*.log"
    args:
      - {{ name: service, type: varchar, required: true }}
    returns:
      - {{ name: file_name, type: varchar }}
"#,
        dir.display()
    );

    let svc = engine_for(cfg_yaml(&yaml), dir.clone()).await;

    // The `{service}` placeholder is filled from the call arg, so only `app/`
    // files match.
    let batches = svc
        .query_collect("SELECT file_name FROM ops.logs('app') ORDER BY file_name")
        .await
        .expect("query");
    let names: Vec<String> = batches
        .iter()
        .flat_map(|b| string_col(b, "file_name"))
        .collect();
    assert_eq!(names, vec!["one.log".to_string(), "two.log".to_string()]);

    // A WHERE filter applies on top of the function result.
    let batches = svc
        .query_collect("SELECT file_name FROM ops.logs('app') WHERE file_name = 'two.log'")
        .await
        .expect("query");
    let names: Vec<String> = batches
        .iter()
        .flat_map(|b| string_col(b, "file_name"))
        .collect();
    assert_eq!(names, vec!["two.log".to_string()]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn unknown_function_call_is_a_clear_error() {
    let dir = tempdir("unknown");
    let svc = engine_for(cfg_yaml("version: 1"), dir.clone()).await;
    let err = svc
        .query_collect("SELECT * FROM nope.missing('x')")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("nope.missing"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn i64_col(batch: &arrow_array::RecordBatch, name: &str) -> Vec<i64> {
    use arrow_array::Array;
    let idx = batch.schema().index_of(name).unwrap();
    let a = batch
        .column(idx)
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .unwrap();
    (0..a.len()).map(|i| a.value(i)).collect()
}

#[tokio::test]
async fn builtin_file_grep_searches_contents() {
    let dir = tempdir("grep");
    std::fs::write(
        dir.join("a.log"),
        "INFO start\nERROR boom\nINFO ok\nERROR again\n",
    )
    .unwrap();
    std::fs::write(dir.join("b.log"), "all good\nERROR in b\n").unwrap();
    std::fs::write(dir.join("c.txt"), "ERROR not a log\n").unwrap(); // excluded by *.log

    let svc = engine_for(cfg_yaml("version: 1"), dir.clone()).await;

    // Matching lines across the .log files, with path + line number.
    let sql = format!(
        "SELECT path, line_number, line FROM file.grep('ERROR', '{}/*.log') ORDER BY path, line_number",
        dir.display()
    );
    let b = svc.query_collect(&sql).await.expect("query");
    assert_eq!(
        string_col_all(&b, "line"),
        vec!["ERROR boom", "ERROR again", "ERROR in b"]
    );
    assert_eq!(i64_col_all(&b, "line_number"), vec![2, 4, 2]);

    // It's a real regex, not a substring match.
    let sql = format!(
        "SELECT line FROM file.grep('^INFO', '{}/a.log') ORDER BY line_number",
        dir.display()
    );
    let b = svc.query_collect(&sql).await.expect("query");
    assert_eq!(string_col_all(&b, "line"), vec!["INFO start", "INFO ok"]);

    // Composes with SQL: WHERE/LIMIT on top.
    let sql = format!(
        "SELECT count(*) AS n FROM (SELECT * FROM file.grep('ERROR', '{}/*.log') LIMIT 2)",
        dir.display()
    );
    let b = svc.query_collect(&sql).await.expect("query");
    assert_eq!(i64_col(&b[0], "n"), vec![2]);

    let _ = std::fs::remove_dir_all(&dir);
}

fn string_col_all(batches: &[arrow_array::RecordBatch], name: &str) -> Vec<String> {
    batches.iter().flat_map(|b| string_col(b, name)).collect()
}
fn i64_col_all(batches: &[arrow_array::RecordBatch], name: &str) -> Vec<i64> {
    batches.iter().flat_map(|b| i64_col(b, name)).collect()
}
