//! Acceptance: spin a `MockEngine` behind `pawrly-server::serve_in_process`,
//! connect a `RemoteEngineClient`, and exercise every service.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use pawrly_client::{Endpoint, RemoteEngineClient, TlsConfig};
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

/// Start a bearer-protected server on an ephemeral loopback port; bind first so
/// the OS accepts connections into the backlog and the client never races the
/// serve loop. Returns the bound address.
async fn spawn_bearer_server(token: &str) -> std::net::SocketAddr {
    use pawrly_server::AuthMode;
    let mock = Arc::new(MockEngine::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = ServerBuilder::new(mock as Arc<dyn EngineService>).auth(AuthMode::Bearer {
        token: token.to_string(),
    });
    tokio::spawn(async move {
        let _ = server.serve_tcp_incoming(listener).await;
    });
    addr
}

#[tokio::test]
async fn bearer_token_required_over_tcp() {
    let addr = spawn_bearer_server("s3cret-token").await;

    // Correct token: requests succeed.
    let ok: Arc<dyn EngineService> = Arc::new(
        RemoteEngineClient::connect(Endpoint::Tcp {
            addr,
            bearer: Some("s3cret-token".into()),
            tls: None,
        })
        .await
        .expect("connect with token"),
    );
    assert!(ok.health().await.expect("health with token").ok);

    // Wrong token: the interceptor rejects with Unauthenticated.
    let bad: Arc<dyn EngineService> = Arc::new(
        RemoteEngineClient::connect(Endpoint::Tcp {
            addr,
            bearer: Some("wrong".into()),
            tls: None,
        })
        .await
        .expect("connect with wrong token"),
    );
    let err = bad
        .health()
        .await
        .expect_err("wrong token must be rejected");
    assert!(
        err.to_string().to_lowercase().contains("bearer"),
        "expected a bearer-auth error, got {err:?}"
    );

    // No token at all: also rejected.
    let none: Arc<dyn EngineService> = Arc::new(
        RemoteEngineClient::connect(Endpoint::Tcp {
            addr,
            bearer: None,
            tls: None,
        })
        .await
        .expect("connect without token"),
    );
    assert!(
        none.health().await.is_err(),
        "missing token must be rejected"
    );
}

#[tokio::test]
async fn tls_round_trip_with_self_signed_cert() {
    // A self-signed cert for "localhost", written to temp PEM files.
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(["localhost".to_string()]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, key_pair.serialize_pem()).unwrap();

    let mock = Arc::new(MockEngine::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server =
        ServerBuilder::new(mock as Arc<dyn EngineService>).tls(cert_path.clone(), key_path);
    tokio::spawn(async move {
        let _ = server.serve_tcp_incoming(listener).await;
    });

    // Client trusts the self-signed cert as its CA; the cert's SAN is
    // "localhost" while we dial 127.0.0.1, so override the server name.
    let tls = TlsConfig {
        ca_cert: Some(cert_path),
        domain_name: Some("localhost".into()),
        ..Default::default()
    };
    let client: Arc<dyn EngineService> = Arc::new(
        RemoteEngineClient::connect(Endpoint::Tcp {
            addr,
            bearer: None,
            tls: Some(tls),
        })
        .await
        .expect("TLS connect"),
    );
    assert!(client.health().await.expect("health over TLS").ok);

    // A plaintext client must not be able to talk to the TLS server.
    let plain = RemoteEngineClient::connect(Endpoint::Tcp {
        addr,
        bearer: None,
        tls: None,
    })
    .await;
    let plaintext_failed = match plain {
        Ok(c) => {
            let c: Arc<dyn EngineService> = Arc::new(c);
            c.health().await.is_err()
        }
        Err(_) => true,
    };
    assert!(
        plaintext_failed,
        "a plaintext client must not talk to a TLS server"
    );
}
