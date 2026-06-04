//! Source dispatch table. Each `kind:` maps to one register function which
//! knows how to install the source's tables on a DataFusion `SessionContext`.

use datafusion::catalog::CatalogProvider;
use datafusion::execution::context::SessionContext;
use pawrly_core::{ConfigError, SourceDef, SourceKind};

// SourceKind imported only for use in match arms.
#[allow(dead_code)]
const _SK: Option<SourceKind> = None;

/// Per-kind summary that describes the tables the source registered.
#[derive(Debug, Clone, Default)]
pub struct RegisterReport {
    pub table_count: u64,
    pub tables: Vec<TableSummary>,
}

#[derive(Debug, Clone)]
pub struct TableSummary {
    pub name: String,
    pub description: Option<String>,
    pub required_filters: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    #[error("{0}")]
    Other(String),
}

/// Register a single source's tables. Dispatch happens here.
pub async fn register_source(
    def: &SourceDef,
    ctx: &SessionContext,
    catalog: &dyn CatalogProvider,
    workspace_dir: &std::path::Path,
    pool: &std::sync::Arc<crate::duckdb_pool::DuckDbPool>,
) -> Result<RegisterReport, RegisterError> {
    match def.kind {
        // Local files use DataFusion's native readers. A `file` source with a
        // `config.storage` block (or a remote-scheme path) reads from an object
        // store and is routed to the DuckDB object-store path instead.
        SourceKind::File if !file_is_remote(def) => {
            // Validate first so the user gets a config error, not an IO error.
            pawrly_sources_duckdb::validate_file_def(def)?;
            let report =
                pawrly_sources_duckdb::register_file_source(def, ctx, catalog, workspace_dir)
                    .await
                    .map_err(|e| RegisterError::Other(e.to_string()))?;
            Ok(RegisterReport {
                table_count: report.table_count,
                tables: report
                    .tables
                    .into_iter()
                    .map(|t| TableSummary {
                        name: t.name,
                        description: t.description,
                        required_filters: Vec::new(),
                    })
                    .collect(),
            })
        }
        SourceKind::Http => {
            let report = pawrly_sources_http::register_http_source(def, ctx, catalog)
                .await
                .map_err(|e| RegisterError::Other(e.to_string()))?;
            let mut tables: Vec<TableSummary> = report
                .tables
                .into_iter()
                .map(|t| TableSummary {
                    name: t.name,
                    description: t.description,
                    required_filters: t.required_filters,
                })
                .collect();
            if report.raw_table_registered {
                tables.push(TableSummary {
                    name: def.name.clone(),
                    description: Some("raw HTTP escape hatch".into()),
                    required_filters: vec!["request_path".into()],
                });
            }
            Ok(RegisterReport {
                table_count: report.table_count,
                tables,
            })
        }
        SourceKind::Sqlite => {
            let report = pawrly_sources_duckdb::register_sqlite_source(def, ctx, catalog)
                .await
                .map_err(|e| RegisterError::Other(e.to_string()))?;
            Ok(RegisterReport {
                table_count: report.table_count,
                tables: report
                    .tables
                    .into_iter()
                    .map(|t| TableSummary {
                        name: t.name,
                        description: t.description,
                        required_filters: Vec::new(),
                    })
                    .collect(),
            })
        }
        // First-class builtins on the in-process DuckDB pool: foreign-DB ATTACH
        // (postgres / mysql / snowflake / duckdb), lakehouse scans (iceberg /
        // delta / ducklake), and object-store reads (remote `file`).
        SourceKind::File
        | SourceKind::Postgres
        | SourceKind::Mysql
        | SourceKind::Duckdb
        | SourceKind::Snowflake
        | SourceKind::Iceberg
        | SourceKind::Ducklake
        | SourceKind::Delta => {
            crate::duckdb_source::register_duckdb_source(def, pool, catalog, workspace_dir).await
        }
    }
}

/// A `file` source reads from an object store (vs the local filesystem) when it
/// declares a `config.storage` block or any path uses a remote URL scheme.
fn file_is_remote(def: &SourceDef) -> bool {
    if def.config.get("storage").is_some() {
        return true;
    }
    let is_remote_path = |v: &serde_json::Value| {
        v.get("path")
            .or_else(|| v.get("location"))
            .and_then(|p| p.as_str())
            .is_some_and(|p| {
                let p = p.to_ascii_lowercase();
                p.starts_with("s3://")
                    || p.starts_with("gs://")
                    || p.starts_with("gcs://")
                    || p.starts_with("az://")
                    || p.starts_with("azure://")
                    || p.starts_with("abfss://")
            })
    };
    is_remote_path(&def.config) || def.tables.iter().any(|t| is_remote_path(&t.config))
}
