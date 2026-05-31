//! In-process Pawrly engine.
//!
//! `LocalEngine` implements [`pawrly_core::EngineService`] using
//! DataFusion as the planner/executor. DuckDB-extension support is not yet wired;
//! the file source (parquet/csv/json) is implemented through DataFusion
//! native readers.

#![doc(html_root_url = "https://docs.rs/pawrly-engine")]

mod cache;
mod duckdb_pool;
mod local;
pub mod optimizer;
mod preagg;
mod registry;
mod stream;

pub use cache::{CacheManager, CachedTableProvider, ManifestEntry};
pub use duckdb_pool::DuckDbPool;
pub use local::{LocalEngine, LocalEngineConfig};
