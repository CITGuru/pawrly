//! In-process Pawrly engine.
//!
//! `LocalEngine` implements [`pawrly_core::EngineService`] using
//! DataFusion as the planner/executor. Local file sources (parquet/csv/json)
//! are read through DataFusion native readers; sources that DuckDB already
//! speaks — Postgres, MySQL, Snowflake, Iceberg/Delta, DuckLake, and remote
//! files over `httpfs` — run through an in-process DuckDB sub-engine
//! (extensions loaded on demand) and surface as DataFusion table providers.

#![doc(html_root_url = "https://docs.rs/pawrly-engine")]

mod activity;
mod cache;
mod duckdb_pool;
mod duckdb_source;
mod durable_activity;
mod json_udf;
mod local;
mod namespace;
pub mod optimizer;
mod preagg;
mod redact;
mod registry;
mod stream;
mod system_table;

pub use cache::{CacheManager, CachedTableProvider, ManifestEntry};
pub use duckdb_pool::DuckDbPool;
pub use local::{LocalEngine, LocalEngineConfig};
