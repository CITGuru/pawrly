//! The `EngineService` trait — the contract every Pawrly engine implementation
//! satisfies. CLI, MCP, library users, and the gRPC server all program against
//! this trait, never against a concrete implementation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use arrow_array::RecordBatch;
use async_trait::async_trait;
use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::cache::{CacheEntryInfo, RefreshOutcome, VacuumReport};
use crate::error::EngineError;
use crate::schema::{CatalogSnapshot, TableDescription, TableFilter, TableInfo, TableName};
use crate::semantic::{SemanticModelDescription, SemanticModelInfo, SemanticQuery};
use crate::source::{
    HealthReport, RefreshCatalogOutcome, ReloadReport, SourceDef, SourceInfo, SourceTestReport,
};

/// Opaque identifier for an in-flight query.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QueryId(pub String);

impl QueryId {
    /// Construct from a raw string (typically a UUID).
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl std::fmt::Display for QueryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Inputs to a `query` call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryRequest {
    pub sql: String,
    /// Substitutions for `${param:KEY}` in the SQL.
    #[serde(default)]
    pub params: HashMap<String, String>,
    /// Override for the engine's default timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "humantime_serde::option")]
    pub timeout: Option<Duration>,
    /// Cap on returned rows. 0 = unlimited.
    #[serde(default)]
    pub max_rows: u64,
    /// Optional client-supplied trace id for log correlation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Who/where this request came from, for activity attribution and trace
    /// propagation. Defaults to in-process; old clients send the default.
    #[serde(default)]
    pub context: crate::activity::RequestContext,
}

impl QueryRequest {
    /// Build a request from a SQL string with no params.
    pub fn sql(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            ..Self::default()
        }
    }
}

/// The streaming output of a `query` call. Each item is one Arrow batch
/// (or a per-batch error). The schema can be obtained from the first batch.
pub type QueryStream = Pin<Box<dyn Stream<Item = Result<RecordBatch, EngineError>> + Send>>;

/// The reserved schema/`source` name that materialized tables live under. No
/// data source may use this name (enforced in the config validator and
/// `add_source`), so `{source names} ∪ {materialized}` is always disjoint and a
/// materialized table is addressable as `<namespace>.materialized.<name>`.
pub const MATERIALIZED_SCHEMA: &str = "materialized";

/// Tabular format of a `File` / `Url` / `Inline` materialize origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaterializeFormat {
    Parquet,
    Csv,
    Json,
}

impl MaterializeFormat {
    /// Infer the format from a path/URL extension (`.parquet`/`.csv`/`.json`/
    /// `.ndjson`/`.jsonl`). Returns `None` for an unknown or missing extension.
    #[must_use]
    pub fn from_path(path: &str) -> Option<Self> {
        let ext = path.rsplit('.').next()?.to_ascii_lowercase();
        match ext.as_str() {
            "parquet" => Some(Self::Parquet),
            "csv" => Some(Self::Csv),
            "json" | "ndjson" | "jsonl" => Some(Self::Json),
            _ => None,
        }
    }
}

/// How a materialized table is produced. The single write verb
/// ([`EngineService::materialize`]) unifies every origin under "produce Arrow
/// batches, write a self-backed Parquet table."
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MaterializeSpec {
    /// Run a SQL query and persist the result.
    Query {
        sql: String,
        /// Substitutions for `${param:KEY}` in the SQL, baked into the stored
        /// spec at materialization time (v1: values are substituted, not
        /// re-bound on a future refresh).
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        params: HashMap<String, String>,
    },
    /// Read a local file (CSV/Parquet/JSON). `format` is inferred from the
    /// extension when `None`.
    File {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<MaterializeFormat>,
    },
    /// Read a remote `http(s)://` file via DuckDB httpfs. `format` is inferred
    /// from the URL extension when `None`.
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<MaterializeFormat>,
    },
    /// Persist inline bytes of a known `format` (no extension to infer from).
    Inline {
        #[serde(with = "inline_bytes")]
        bytes: Vec<u8>,
        format: MaterializeFormat,
    },
}

/// Base64 (de)serialization for inline bytes so a `MaterializeSpec` stays
/// JSON/YAML-friendly when stored in the manifest origin.
mod inline_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        use base64::Engine as _;
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

/// The artifact returned by [`EngineService::materialize`]: a self-backed table
/// queryable by name. `name` is `("materialized", <name>)`, addressable in SQL
/// as `<namespace>.materialized.<name>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterializeOutcome {
    /// The manifest key — `materialized.<name>`.
    pub name: TableName,
    /// The on-disk Parquet file backing the table.
    pub file_path: PathBuf,
    pub row_count: u64,
    pub size_bytes: u64,
}

/// The single contract every Pawrly engine implementation satisfies.
///
/// Implementations:
///
/// - `LocalEngine` (`pawrly-engine`): runs DataFusion + DuckDB in-process.
/// - `RemoteEngineClient` (`pawrly-client`): talks to `pawrly serve` over gRPC.
/// - `MockEngine` (this crate, behind the `test-support` feature): in-memory
///   canned responses for testing.
///
/// Every method maps 1:1 to a gRPC RPC defined in `pawrly-proto`.
#[async_trait]
pub trait EngineService: Send + Sync + 'static {
    // -------- query --------

    /// Execute a SQL query and return a streaming result.
    async fn query(&self, req: QueryRequest) -> Result<QueryStream, EngineError>;

    /// Return the optimized plan for a SQL string. If `analyze` is true,
    /// the plan is executed and timings are included.
    async fn explain(&self, sql: &str, analyze: bool) -> Result<String, EngineError>;

    /// Cancel an in-flight query. Returns `true` if a query with the given
    /// id was found and signaled.
    async fn cancel(&self, query_id: &QueryId) -> Result<bool, EngineError>;

    // -------- catalog --------

    async fn list_sources(&self) -> Result<Vec<SourceInfo>, EngineError>;

    async fn list_tables(&self, filter: Option<TableFilter>)
    -> Result<Vec<TableInfo>, EngineError>;

    async fn describe_table(&self, name: &TableName) -> Result<TableDescription, EngineError>;

    async fn schema_snapshot(
        &self,
        sources: Option<Vec<String>>,
        compact: bool,
    ) -> Result<CatalogSnapshot, EngineError>;

    async fn refresh_catalog(
        &self,
        source: Option<&str>,
    ) -> Result<RefreshCatalogOutcome, EngineError>;

    // -------- cache --------

    async fn cache_entries(&self) -> Result<Vec<CacheEntryInfo>, EngineError>;

    async fn refresh_table(&self, name: &TableName) -> Result<RefreshOutcome, EngineError>;

    async fn invalidate_cache(&self, name: &TableName) -> Result<bool, EngineError>;

    async fn vacuum_cache(&self) -> Result<VacuumReport, EngineError>;

    // -------- materialized tables --------

    /// Run `spec` and persist its result as a self-backed Parquet table named
    /// `name`, queryable as `<namespace>.materialized.<name>`. Pinned (never
    /// auto-reclaimed by TTL/vacuum); create-or-replace by name.
    async fn materialize(
        &self,
        name: &str,
        spec: MaterializeSpec,
    ) -> Result<MaterializeOutcome, EngineError>;

    /// Drop a materialized table (entry + file). Returns `false` if no such
    /// table existed. Only acts on `materialized.<name>`.
    async fn drop_materialized(&self, name: &str) -> Result<bool, EngineError>;

    // -------- source mgmt --------

    async fn add_source(&self, def: SourceDef) -> Result<SourceInfo, EngineError>;

    async fn remove_source(&self, name: &str) -> Result<bool, EngineError>;

    async fn test_source(&self, name: &str) -> Result<SourceTestReport, EngineError>;

    async fn reload_config(&self) -> Result<ReloadReport, EngineError>;

    // -------- semantic --------

    /// List the semantic models defined in the workspace.
    async fn list_semantic_models(&self) -> Result<Vec<SemanticModelInfo>, EngineError>;

    /// Return the full spec (dimensions, measures) for one model.
    async fn describe_semantic_model(
        &self,
        name: &str,
    ) -> Result<SemanticModelDescription, EngineError>;

    /// Compile and execute a structured query, returning a streaming result
    /// in the same shape as [`EngineService::query`].
    async fn semantic_query(&self, q: SemanticQuery) -> Result<QueryStream, EngineError>;

    // -------- lifecycle --------

    async fn health(&self) -> Result<HealthReport, EngineError>;

    async fn shutdown(&self) -> Result<(), EngineError>;
}

/// Convenience methods provided as default implementations on top of
/// [`EngineService`]. Available via `use pawrly_core::EngineServiceExt;`.
#[async_trait]
pub trait EngineServiceExt: EngineService {
    /// Run a query and collect every batch into a `Vec`. Convenient for tests
    /// and small queries; do **not** use for large result sets.
    async fn query_collect(&self, sql: &str) -> Result<Vec<RecordBatch>, EngineError> {
        use futures_util::StreamExt as _;

        let mut stream = self.query(QueryRequest::sql(sql)).await?;
        let mut out = Vec::new();
        while let Some(batch) = stream.next().await {
            out.push(batch?);
        }
        Ok(out)
    }

    /// Run a query expecting at most one batch; return `None` if the query
    /// produces no rows, or `Some(first_batch)` otherwise. Subsequent batches
    /// are dropped silently — only use when the caller knows the cardinality.
    async fn query_one(&self, sql: &str) -> Result<Option<RecordBatch>, EngineError> {
        let batches = self.query_collect(sql).await?;
        Ok(batches.into_iter().next())
    }

    /// Run a [`SemanticQuery`](crate::semantic::SemanticQuery) and collect every
    /// batch. Same caveat as [`query_collect`](Self::query_collect): for small
    /// result sets only.
    async fn semantic_query_collect(
        &self,
        q: crate::semantic::SemanticQuery,
    ) -> Result<Vec<RecordBatch>, EngineError> {
        use futures_util::StreamExt as _;

        let mut stream = self.semantic_query(q).await?;
        let mut out = Vec::new();
        while let Some(batch) = stream.next().await {
            out.push(batch?);
        }
        Ok(out)
    }
}

impl<T: EngineService + ?Sized> EngineServiceExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_request_builder() {
        let r = QueryRequest::sql("SELECT 1");
        assert_eq!(r.sql, "SELECT 1");
        assert!(r.params.is_empty());
        assert_eq!(r.max_rows, 0);
    }

    #[test]
    fn query_id_round_trip() {
        let id = QueryId::new("abc-123");
        assert_eq!(format!("{id}"), "abc-123");
    }
}
