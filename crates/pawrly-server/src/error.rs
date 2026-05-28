//! Server-level errors. The `EngineError -> tonic::Status` conversion lives
//! in `pawrly-proto::conv` so the client can use the inverse direction too.

pub use pawrly_proto::conv::engine_error_to_status;

/// Errors that can arise inside the server itself (not query-level errors).
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("tonic transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("non-loopback TCP requires AuthMode::Bearer")]
    AuthRequiredForNonLoopback,
}
