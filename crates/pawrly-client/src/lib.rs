//! `RemoteEngineClient`: a gRPC-backed implementation of `EngineService`.
//!
//! Connect via [`Endpoint`] (`Tcp`, `Uds`, or `InProcess`); the client
//! satisfies the same `EngineService` trait as `LocalEngine`, so every
//! frontend (CLI, MCP, library) can treat them interchangeably.

#![doc(html_root_url = "https://docs.rs/pawrly-client")]

mod remote;
mod transport;

pub use remote::RemoteEngineClient;
pub use transport::{ConnectError, Endpoint, TlsConfig};
