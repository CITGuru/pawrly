//! Activity log: one structured record per engine operation.
//!
//! The engine builds an [`ActivityRecord`] at the completion of each operation
//! and hands it to an [`ActivityRecorder`]. The default [`NoopRecorder`] drops
//! everything, matching "off by default" — real recorders (the `tracing` event
//! sink and the `system.activity` table) live in `pawrly-engine`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The interface a request entered the engine through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interface {
    /// In-process library/CLI call with no wire hop (the default for old
    /// clients that send no context).
    #[default]
    InProcess,
    /// CLI front-end.
    Cli,
    /// gRPC daemon.
    Grpc,
    /// MCP (stdio or HTTP).
    Mcp,
    /// Arrow Flight.
    Flight,
}

impl Interface {
    /// Stable lowercase identifier for logs, metric attributes, and the
    /// `system.activity` column.
    pub fn as_str(self) -> &'static str {
        match self {
            Interface::InProcess => "in_process",
            Interface::Cli => "cli",
            Interface::Grpc => "grpc",
            Interface::Mcp => "mcp",
            Interface::Flight => "flight",
        }
    }
}

/// The engine operation an activity record describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// A SQL `query`.
    Query,
    /// A `semantic_query`.
    SemanticQuery,
    /// An `explain`.
    Explain,
    /// A `materialize`.
    Materialize,
}

impl Operation {
    /// Stable lowercase identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Operation::Query => "query",
            Operation::SemanticQuery => "semantic_query",
            Operation::Explain => "explain",
            Operation::Materialize => "materialize",
        }
    }
}

/// Terminal status of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Completed successfully.
    Ok,
    /// Failed.
    Error,
}

impl Status {
    /// Stable lowercase identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Error => "error",
        }
    }
}

/// One record per engine operation, emitted from a single choke point so every
/// sink sees identical data. SQL is redacted per policy before it reaches here;
/// only param **keys** are ever recorded, never values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityRecord {
    /// Operation id; equals the `query_id` where applicable.
    pub id: String,
    /// When the operation completed.
    pub at: DateTime<Utc>,
    /// The interface it arrived through.
    pub interface: Interface,
    /// Authenticated identity, when known.
    pub principal: Option<String>,
    /// Which operation this was.
    pub operation: Operation,
    /// Redacted SQL text, or `None` when redaction stores no text.
    pub sql: Option<String>,
    /// Parameter keys only — never values.
    pub param_keys: Vec<String>,
    /// Terminal status.
    pub status: Status,
    /// Engine error code, when `status` is `Error`.
    pub error_code: Option<String>,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Rows returned, when applicable and known.
    pub rows_returned: Option<u64>,
    /// Bytes produced, when known.
    pub bytes: Option<u64>,
    /// OTel trace id for cross-referencing, when a trace was sampled.
    pub trace_id: Option<String>,
}

/// Sink for [`ActivityRecord`]s. Implementations must be cheap and non-blocking
/// on the calling path; durable work belongs on a background task.
#[async_trait]
pub trait ActivityRecorder: Send + Sync {
    /// Record one activity entry.
    async fn record(&self, rec: ActivityRecord);
}

/// Recorder that drops every record. The engine's default, so activity logging
/// is off until a real recorder is installed.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRecorder;

#[async_trait]
impl ActivityRecorder for NoopRecorder {
    async fn record(&self, _rec: ActivityRecord) {}
}

/// Per-request context the frontends populate so the engine can attribute an
/// operation. Carried on [`crate::service::QueryRequest`]; old clients send the
/// default (`InProcess`, no principal, no traceparent), preserving behaviour.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestContext {
    /// The interface the request arrived through.
    pub interface: Interface,
    /// Authenticated identity, when known.
    pub principal: Option<String>,
    /// W3C `traceparent` for distributed-trace correlation.
    pub traceparent: Option<String>,
}
