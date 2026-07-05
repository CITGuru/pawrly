//! Integration: the `connect()` facade over both the gRPC and REST transports,
//! each driven against the real server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pawrly_client::{Endpoint, connect};
use pawrly_core::EngineServiceExt as _;
use pawrly_core::test_support::MockEngine;
use pawrly_server::{ConsoleOpts, ServerBuilder};

/// Grab an ephemeral port by binding then releasing it.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A mock engine with one canned `SELECT` row.
fn mock() -> MockEngine {
    let engine = MockEngine::new();
    engine.canned("SELECT", vec![MockEngine::one_row(1, "a")]);
    engine
}

#[tokio::test]
async fn rest_transport_round_trip() {
    let port = free_port();
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tokio::spawn(async move {
        let _ = ServerBuilder::new(Arc::new(mock()))
            .serve_console(ConsoleOpts {
                addr,
                cors_origin: None,
            })
            .await;
    });

    let client = connect(Endpoint::Rest {
        base_url: format!("http://127.0.0.1:{port}"),
        bearer: None,
    })
    .await
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    while client.health().await.is_err() {
        assert!(Instant::now() < deadline, "rest server never came up");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(client.health().await.unwrap().ok);
    let rows: usize = client
        .query_collect("SELECT 1")
        .await
        .unwrap()
        .iter()
        .map(|b| b.num_rows())
        .sum();
    assert_eq!(rows, 1);
    assert!(client.list_tables(None).await.is_ok());

    // shutdown is unsupported over REST.
    assert_eq!(
        client.shutdown().await.unwrap_err().code(),
        "PAWRLY_UNSUPPORTED"
    );
}

#[tokio::test]
async fn grpc_transport_round_trip() {
    let port = free_port();
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tokio::spawn(async move {
        let _ = ServerBuilder::new(Arc::new(mock())).serve_tcp(addr).await;
    });

    // gRPC connects eagerly; retry until the server is listening.
    let deadline = Instant::now() + Duration::from_secs(5);
    let client = loop {
        match connect(Endpoint::Tcp {
            addr,
            bearer: None,
            tls: None,
        })
        .await
        {
            Ok(c) => break c,
            Err(_) => {
                assert!(Instant::now() < deadline, "grpc server never came up");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };

    assert!(client.health().await.unwrap().ok);
    let rows: usize = client
        .query_collect("SELECT 1")
        .await
        .unwrap()
        .iter()
        .map(|b| b.num_rows())
        .sum();
    assert_eq!(rows, 1);
    assert!(client.list_tables(None).await.is_ok());
}
