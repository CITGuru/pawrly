//! `query`/`semantic_query` return a `QueryHandle` that exposes the cancel `id`
//! and a `completion` slot, and `cancel(id)` terminates an in-flight query with
//! a `PAWRLY_CANCELLED` error.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use futures_util::StreamExt as _;
use pawrly_core::{EngineService, QueryId, QueryRequest};
use pawrly_engine::{LocalEngine, LocalEngineConfig};

fn cfg_yaml(yaml: &str) -> pawrly_config::Config {
    let secrets = pawrly_secrets::StaticStore::new();
    pawrly_config::load_str(yaml, &secrets).unwrap()
}

async fn engine(workspace: &std::path::Path) -> Arc<dyn EngineService> {
    Arc::new(
        LocalEngine::new(LocalEngineConfig {
            config: cfg_yaml("version: 1\n"),
            workspace_dir: workspace.to_path_buf(),
            duckdb_pool_size: None,
            home: None,
        })
        .await
        .unwrap(),
    )
}

#[tokio::test]
async fn query_handle_carries_id_and_completion() {
    let tmp = tempfile::TempDir::new().unwrap();
    let engine = engine(tmp.path()).await;

    let handle = engine
        .query(QueryRequest::sql("SELECT 1 AS n"))
        .await
        .unwrap();
    assert!(!handle.id.0.is_empty(), "query id should be assigned");

    let mut stream = handle.stream;
    let mut rows = 0;
    while let Some(batch) = stream.next().await {
        rows += batch.unwrap().num_rows();
    }
    assert_eq!(rows, 1);

    let done = handle
        .completion
        .get()
        .expect("completion is filled after the stream ends");
    assert_eq!(done.rows_returned, 1);
    assert!(!done.truncated);
}

#[tokio::test]
async fn cancel_unknown_id_returns_false() {
    let tmp = tempfile::TempDir::new().unwrap();
    let engine = engine(tmp.path()).await;
    assert!(!engine.cancel(&QueryId::new("no-such-query")).await.unwrap());
}

#[tokio::test]
async fn cancel_terminates_in_flight_query() {
    let tmp = tempfile::TempDir::new().unwrap();
    let engine = engine(tmp.path()).await;

    let handle = engine
        .query(QueryRequest::sql("SELECT 1 AS n"))
        .await
        .unwrap();

    assert!(
        engine.cancel(&handle.id).await.unwrap(),
        "cancel should find the in-flight query"
    );

    // First poll observes the flag → one Cancelled error, then the stream ends.
    let mut stream = handle.stream;
    let err = stream
        .next()
        .await
        .expect("one terminal item")
        .expect_err("a cancelled query yields an error");
    assert_eq!(err.code(), "PAWRLY_CANCELLED");
    assert!(stream.next().await.is_none(), "stream ends after cancel");

    // The id is deregistered once the stream is done, so a second cancel misses.
    assert!(!engine.cancel(&handle.id).await.unwrap());
}
