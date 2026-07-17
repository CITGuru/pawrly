//! Arrow Flight SQL transport for Pawrly.
//!
//! This crate is the second transport in Pawrly's pluggable-transport model.
//! It will expose any `EngineService` over the Arrow Flight SQL wire
//! protocol — letting `pyarrow.flight`, Dremio,
//! Tableau, Power BI, DuckDB's Flight client, and ADBC drivers talk to a
//! Pawrly engine the same way they'd talk to any Flight SQL database.
//!
//! ## Design
//!
//! A `FlightSqlServerBuilder::new(engine).serve_tcp(addr)` mirrors the shape
//! of `pawrly_server::ServerBuilder`, but produces a Flight SQL surface:
//!
//! | Flight SQL RPC            | `EngineService` method        | Status   |
//! | ------------------------- | ----------------------------- | -------- |
//! | `ExecuteStatement`        | `query`                       | planned  |
//! | `GetTables`               | `list_tables`                 | planned  |
//! | `GetSchema`               | `describe_table`              | planned  |
//! | `GetCatalogs`             | synthetic; always `["pawrly"]`| planned  |
//! | `GetDbSchemas`            | `list_sources`                | planned  |
//! | `GetSqlInfo`              | synthetic capabilities        | planned  |
//! | `ExecuteUpdate`           | read-only → refused           | n/a      |
//!
//! Custom surfaces that don't fit Flight SQL (`SourcesService`,
//! `CacheService`, `AdminService`) stay on the plain-tonic `pawrly-server`
//! transport. An operator running both can expose Flight SQL for BI tools
//! and custom gRPC for CLI/MCP — same `EngineService` powers both.
//!
//! ## Status
//!
//! The crate skeleton + `arrow-flight` dependency are wired in
//! so the transport-plugin architecture is real in the workspace. The
//! actual Flight SQL service implementation is a dedicated follow-up;
//! the design is stable enough that it can ship without touching any
//! consumer code (consumers still hold `Arc<dyn EngineService>`).

#![doc(html_root_url = "https://docs.rs/pawrly-flight")]

use std::sync::Arc;

use pawrly_core::EngineService;

/// Placeholder builder — wiring for `FlightSqlServerBuilder::new(engine)`.
///
/// Once implemented it will look exactly like `pawrly_server::ServerBuilder`
/// but produce a `tonic::transport::Server` that serves
/// `arrow_flight::flight_service_server::FlightService` (and the Flight SQL
/// subset of actions) instead of the custom Pawrly protos.
pub struct FlightSqlServerBuilder {
    _engine: Arc<dyn EngineService>,
}

impl FlightSqlServerBuilder {
    /// Construct from an engine implementation.
    #[must_use]
    pub fn new(engine: Arc<dyn EngineService>) -> Self {
        Self { _engine: engine }
    }

    /// Serve over TCP. Not yet implemented.
    ///
    /// # Errors
    ///
    /// Always returns `FlightError::NotImplemented`.
    pub async fn serve_tcp(self, _addr: std::net::SocketAddr) -> Result<(), FlightError> {
        Err(FlightError::NotImplemented)
    }
}

/// Errors raised by the Flight SQL transport.
#[derive(Debug, thiserror::Error)]
pub enum FlightError {
    #[error("Flight SQL transport is scaffolded but not yet implemented")]
    NotImplemented,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_core::test_support::MockEngine;

    #[tokio::test]
    async fn builder_is_wired() {
        let engine: Arc<dyn EngineService> = Arc::new(MockEngine::new());
        let builder = FlightSqlServerBuilder::new(engine);
        let res = builder.serve_tcp("127.0.0.1:0".parse().unwrap()).await;
        assert!(matches!(res, Err(FlightError::NotImplemented)));
    }
}
