//! Pawrly gRPC API: protobuf contract and tonic-generated bindings.
//!
//! Re-exports the generated `pawrly.v1` module under [`v1`]. Also provides
//! Arrow IPC helpers in [`arrow_helpers`] used by both `pawrly-server` and
//! `pawrly-client`.

#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    missing_docs,
    reason = "tonic generates code outside our lint scope"
)]

pub mod v1 {
    tonic::include_proto!("pawrly.v1");
}

pub mod arrow_helpers;
pub mod conv;
pub mod propagation;

#[cfg(test)]
mod tests {
    use super::v1;

    #[test]
    fn types_are_reachable() {
        let _ = v1::TableName {
            schema: "x".into(),
            table: "y".into(),
        };
        let _ = v1::QueryRequest {
            sql: "SELECT 1".into(),
            ..Default::default()
        };
        let _ = v1::HealthResponse {
            ok: true,
            version: "0.1.0".into(),
            ..Default::default()
        };
    }
}
