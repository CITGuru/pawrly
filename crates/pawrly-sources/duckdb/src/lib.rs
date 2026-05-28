//! DuckDB-backed (and DataFusion-backed) sources for Pawrly.
//!
//! The file source is implemented via DataFusion's native readers
//! (parquet / csv / json). DuckDB-extension-backed sources are not yet implemented.

#![doc(html_root_url = "https://docs.rs/pawrly-sources-duckdb")]

mod file;
mod sqlite;

pub use file::{
    FileBuildError, FileSourceReport, FileSummary, register_file_source, validate_file_def,
};
pub use sqlite::{
    SqliteBuildError, SqliteSourceReport, SqliteTableSummary, register_sqlite_source,
};
