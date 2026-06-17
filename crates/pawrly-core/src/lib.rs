//! Core types, errors, and the [`EngineService`] trait for Pawrly.
//!
//! This crate has no engine implementation. Every other crate in the workspace
//! programs against the trait and types defined here.

#![doc(html_root_url = "https://docs.rs/pawrly-core")]

pub mod activity;
pub mod cache;
pub mod error;
pub mod home;
pub mod model;
pub mod optimizer;
pub mod safety;
pub mod schema;
pub mod semantic;
pub mod service;
pub mod source;
pub mod storage;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use cache::{CacheEntryInfo, CacheMode, CachePolicy, RefreshOutcome, VacuumReport};
pub use error::{
    ConfigError, ConfigErrors, EngineError, ErrorCode, PawrlyError, SafetyError, SourceError,
};
pub use home::resolve_home;
pub use model::SourceKind;
pub use optimizer::DynamicFilterCapable;
pub use safety::SafetyPolicy;
pub use schema::{
    CatalogSnapshot, ColumnSpec, SchemaSummary, TableDescription, TableFilter, TableInfo,
    TableName, TableSpec, TableSummary,
};
pub use semantic::{
    Dimension, DimensionType, FilterOp, Measure, MeasureAgg, OrderDir, SemanticFilter,
    SemanticModel, SemanticModelDescription, SemanticModelInfo, SemanticOrder, SemanticQuery,
    TimeGrain,
};
pub use service::{
    EngineService, EngineServiceExt, MATERIALIZED_SCHEMA, MaterializeFormat, MaterializeOutcome,
    MaterializeSpec, QueryId, QueryRequest, QueryStream, SYSTEM_SCHEMA,
};
pub use source::{
    HealthReport, RefreshCatalogOutcome, ReloadReport, SourceDef, SourceInfo, SourceStatus,
    SourceTestReport, TableDef,
};
pub use storage::{StorageScheme, origin_prefix};
