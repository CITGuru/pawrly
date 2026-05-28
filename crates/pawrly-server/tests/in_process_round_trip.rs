//! Acceptance: spin a `MockEngine` behind `pawrly-server::serve_in_process`,
//! connect a `RemoteEngineClient`, and exercise every service.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use pawrly_client::{Endpoint, RemoteEngineClient};
use pawrly_core::test_support::MockEngine;
use pawrly_core::{ColumnSpec, EngineService, EngineServiceExt, SourceDef, SourceKind, TableName};
use pawrly_server::ServerBuilder;

#[tokio::test]
async fn full_round_trip_against_mock_engine() {
    // 1. Build a mock engine pre-loaded with a table and a canned query response.
    //    Keep two handles: `mock_typed` so we can inspect it after the test,
    //    and an `Arc<dyn EngineService>` clone for the server.
    let mock_typed = Arc::new(MockEngine::new());
    mock_typed.add_source("data", SourceKind::File);
    mock_typed.add_table(
        TableName::new("data", "orders"),
        SourceKind::File,
        vec![ColumnSpec {
            name: "id".into(),
            data_type: "Int64".into(),
            nullable: false,
            description: None,
            is_filter_pushable: false,
            is_required_filter: false,
        }],
    );
    mock_typed.canned("FROM data.orders", vec![MockEngine::one_row(99, "demo")]);
    let mock: Arc<dyn EngineService> = mock_typed.clone();

    // 2. Spawn the server in-process and grab a tonic Channel for the client.
    let channel = ServerBuilder::new(mock)
        .serve_in_process()
        .await
        .expect("server should start");

    // 3. Wire up the remote client over that in-process channel.
    let client: Arc<dyn EngineService> = Arc::new(
        RemoteEngineClient::connect(Endpoint::InProcess(channel))
            .await
            .expect("client should connect"),
    );

    // 4. Exercise every service.
    let health = client.health().await.expect("health");
    assert!(health.ok);
    assert_eq!(health.sources_ok, 1);

    let sources = client.list_sources().await.expect("list_sources");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].name, "data");

    let tables = client.list_tables(None).await.expect("list_tables");
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name.to_string(), "data.orders");

    let desc = client
        .describe_table(&TableName::new("data", "orders"))
        .await
        .expect("describe");
    assert_eq!(desc.columns.len(), 1);
    assert_eq!(desc.columns[0].name, "id");

    // 5. Streaming query over Arrow IPC.
    let batches = client
        .query_collect("SELECT * FROM data.orders WHERE id = 99")
        .await
        .expect("query");
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 1);
    assert_eq!(batch.num_columns(), 2);

    // 6. The mock engine on the server side recorded the SQL we issued.
    let seen = mock_typed.queries_seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].contains("FROM data.orders"));
}

#[tokio::test]
async fn add_source_round_trip() {
    let mock_typed = Arc::new(MockEngine::new());
    let mock: Arc<dyn EngineService> = mock_typed.clone();

    let channel = ServerBuilder::new(mock)
        .serve_in_process()
        .await
        .expect("server should start");
    let client: Arc<dyn EngineService> = Arc::new(
        RemoteEngineClient::connect(Endpoint::InProcess(channel))
            .await
            .expect("client should connect"),
    );

    let def = SourceDef {
        name: "newsrc".into(),
        kind: SourceKind::File,
        description: Some("added at runtime".into()),
        config: serde_json::json!({ "path": "./data/*.parquet" }),
        cache: Default::default(),
        safety: None,
        tables: Vec::new(),
        raw_table: false,
        raw_table_safety: None,
    };

    // The SourceDef serializes to YAML on the client, crosses the wire, and is
    // re-parsed server-side before reaching the engine.
    let info = client.add_source(def).await.expect("add_source");
    assert_eq!(info.name, "newsrc");
    assert_eq!(info.kind, SourceKind::File);

    let sources = client.list_sources().await.expect("list_sources");
    assert!(sources.iter().any(|s| s.name == "newsrc"));
}

#[tokio::test]
async fn server_rejects_non_loopback_tcp_without_auth() {
    let mock = Arc::new(MockEngine::new());
    let addr: std::net::SocketAddr = "0.0.0.0:0".parse().unwrap();
    let res = ServerBuilder::new(mock as Arc<dyn EngineService>)
        .serve_tcp(addr)
        .await;
    assert!(matches!(
        res,
        Err(pawrly_server::ServerError::AuthRequiredForNonLoopback)
    ));
}
