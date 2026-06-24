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
    /// Live connection handle (http/mcp only), so attached functions can share
    /// the source's rate-limiter / session. `None` for other kinds.
    pub function_handle: crate::functions::SourceHandle,
}

#[derive(Debug, Clone)]
pub struct TableSummary {
    pub name: String,
    pub description: Option<String>,
    /// Agent-facing usage notes from the table declaration (see `TableDef::wiki`).
    pub wiki: Option<String>,
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
    let mut report = match def.kind {
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
                        wiki: None,
                        required_filters: Vec::new(),
                    })
                    .collect(),
                function_handle: crate::functions::SourceHandle::None,
            })
        }
        SourceKind::Http => {
            let report = pawrly_sources_http::register_http_source(def, ctx, catalog)
                .await
                .map_err(|e| RegisterError::Other(e.to_string()))?;
            let function_handle = report
                .source_handle
                .clone()
                .map_or(crate::functions::SourceHandle::None, |h| {
                    crate::functions::SourceHandle::Http(h)
                });
            let mut tables: Vec<TableSummary> = report
                .tables
                .into_iter()
                .map(|t| TableSummary {
                    name: t.name,
                    description: t.description,
                    wiki: None,
                    required_filters: t.required_filters,
                })
                .collect();
            if report.raw_table_registered {
                tables.push(TableSummary {
                    name: def.name.clone(),
                    description: Some("raw HTTP escape hatch".into()),
                    wiki: None,
                    required_filters: vec!["request_path".into()],
                });
            }
            Ok(RegisterReport {
                table_count: report.table_count,
                tables,
                function_handle,
            })
        }
        SourceKind::Mcp => {
            let report = pawrly_sources_mcp::register_mcp_source(def, ctx, catalog)
                .await
                .map_err(|e| RegisterError::Other(e.to_string()))?;
            let function_handle = report
                .session_handle
                .clone()
                .map_or(crate::functions::SourceHandle::None, |h| {
                    crate::functions::SourceHandle::Mcp(h)
                });
            Ok(RegisterReport {
                table_count: report.table_count,
                tables: report
                    .tables
                    .into_iter()
                    .map(|t| TableSummary {
                        name: t.name,
                        description: t.description,
                        wiki: None,
                        required_filters: Vec::new(),
                    })
                    .collect(),
                function_handle,
            })
        }
        SourceKind::Sqlite => {
            let report =
                pawrly_sources_duckdb::register_sqlite_source(def, ctx, catalog, workspace_dir)
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
                        wiki: None,
                        required_filters: Vec::new(),
                    })
                    .collect(),
                function_handle: crate::functions::SourceHandle::None,
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
    }?;

    // Per-kind registration doesn't carry the agent-facing wiki; backfill it
    // from the table declarations.
    for t in &mut report.tables {
        t.wiki = def
            .tables
            .iter()
            .find(|d| d.name == t.name)
            .and_then(|d| d.wiki.clone());
    }
    Ok(report)
}

/// Finer-grained display variant of a source's `kind`, when the bare kind
/// hides a meaningful mode. `None` when the kind already says it all.
#[must_use]
pub fn sub_kind(def: &SourceDef) -> Option<&'static str> {
    match def.kind {
        SourceKind::Http if def.config.get("type").and_then(|v| v.as_str()) == Some("openapi") => {
            Some("openapi")
        }
        SourceKind::File if file_is_remote(def) => Some("object_storage"),
        _ => None,
    }
}

/// A `file` source reads from an object store / http (vs the local filesystem)
/// when it declares a `config.storage` block or any path uses a remote URL
/// scheme (`http(s)://`, `s3://`, `gs://`/`gcs://`, `az://`/`azure://`/`abfss://`).
fn file_is_remote(def: &SourceDef) -> bool {
    if def.config.get("storage").is_some() {
        return true;
    }
    let is_remote_path = |v: &serde_json::Value| {
        v.get("path")
            .or_else(|| v.get("location"))
            .and_then(|p| p.as_str())
            .is_some_and(|p| pawrly_core::StorageScheme::classify(p).is_remote())
    };
    is_remote_path(&def.config) || def.tables.iter().any(|t| is_remote_path(&t.config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_core::TableDef;

    fn file_def(config: serde_json::Value, tables: Vec<TableDef>) -> SourceDef {
        SourceDef {
            name: "f".into(),
            kind: SourceKind::File,
            description: None,
            wiki: None,
            examples: Vec::new(),
            config,
            cache: pawrly_core::CachePolicy::None,
            safety: None,
            tables,
            raw_table: false,
            raw_table_safety: None,
        }
    }

    fn table(path: &str) -> TableDef {
        TableDef {
            name: "t".into(),
            description: None,
            wiki: None,
            config: serde_json::json!({ "path": path }),
            cache: None,
            safety: None,
        }
    }

    #[test]
    fn http_path_is_remote() {
        // http(s) paths route to the object-store reader.
        assert!(file_is_remote(&file_def(
            serde_json::json!({}),
            vec![table("https://h/data.parquet")]
        )));
        assert!(file_is_remote(&file_def(
            serde_json::json!({ "path": "http://h/data.csv" }),
            vec![]
        )));
    }

    #[test]
    fn object_store_schemes_are_remote() {
        for p in [
            "s3://b/k",
            "gs://b/k",
            "gcs://b/k",
            "az://c/k",
            "azure://c/k",
            "abfss://c@a/k",
        ] {
            assert!(
                file_is_remote(&file_def(serde_json::json!({}), vec![table(p)])),
                "{p} should be remote"
            );
        }
    }

    #[test]
    fn local_paths_are_not_remote() {
        assert!(!file_is_remote(&file_def(
            serde_json::json!({ "path": "./data/x.parquet" }),
            vec![]
        )));
        assert!(!file_is_remote(&file_def(
            serde_json::json!({}),
            vec![table("/abs/data/x.parquet")]
        )));
    }

    #[test]
    fn table_level_remote_path_is_remote() {
        // Config-level path is local, but a table points remote.
        assert!(file_is_remote(&file_def(
            serde_json::json!({ "path": "./local" }),
            vec![table("s3://b/k")]
        )));
    }

    #[test]
    fn explicit_storage_block_is_remote_even_with_local_path() {
        assert!(file_is_remote(&file_def(
            serde_json::json!({ "storage": { "type": "s3" }, "path": "./local.parquet" }),
            vec![]
        )));
    }
}
