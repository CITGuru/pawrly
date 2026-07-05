//! `pawrly-client` — transport-agnostic `EngineService` clients.
//!
//! [`connect`] dispatches on an [`Endpoint`]: gRPC (`Tcp`/`Uds`/`InProcess`)
//! builds a [`RemoteEngineClient`]; `Rest` builds a [`RestEngineClient`]. Every
//! client satisfies the same `EngineService` trait as `LocalEngine`, so every
//! frontend (CLI, MCP, library) can treat them interchangeably.

#![doc(html_root_url = "https://docs.rs/pawrly-client")]

use std::sync::Arc;

use pawrly_core::EngineService;

mod remote;
mod rest_client;
mod transport;

pub use remote::RemoteEngineClient;
pub use rest_client::RestEngineClient;
pub use transport::{ConnectError, Endpoint, TlsConfig};

/// Connect to a Pawrly engine over the endpoint's transport, returning a
/// transport-agnostic [`EngineService`] handle.
pub async fn connect(endpoint: Endpoint) -> Result<Arc<dyn EngineService>, ConnectError> {
    match endpoint {
        Endpoint::Rest { base_url, bearer } => {
            Ok(Arc::new(RestEngineClient::new(base_url, bearer)))
        }
        grpc => Ok(Arc::new(RemoteEngineClient::connect(grpc).await?)),
    }
}
