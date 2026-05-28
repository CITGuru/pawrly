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
) -> Result<RegisterReport, RegisterError> {
    match def.kind {
        SourceKind::File => {
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
        // HTTP-shaped kinds: bundled (github / linear / stripe / …) +
        // generic `http`. All share one register function.
        SourceKind::Http
        | SourceKind::Github
        | SourceKind::Linear
        | SourceKind::Stripe
        | SourceKind::Sentry
        | SourceKind::Datadog
        | SourceKind::Slack
        | SourceKind::Notion => {
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
        SourceKind::Ai => {
            let report = pawrly_sources_ai::register_ai_source(def, ctx, catalog)
                .await
                .map_err(|e| RegisterError::Other(e.to_string()))?;
            Ok(RegisterReport {
                table_count: report.table_count,
                tables: vec![TableSummary {
                    name: "models".into(),
                    description: Some("AI models catalog".into()),
                    required_filters: Vec::new(),
                }],
            })
        }
        // Warehouse + lakehouse + object stores are recognized but
        // require optional features (DuckDB extensions, delta-rs, iceberg-rs)
        // that aren't enabled in this build. The architecture supports them
        // via the same dispatch table; turning them on is a per-deployment
        // build-time choice.
        SourceKind::Snowflake
        | SourceKind::Bigquery
        | SourceKind::Redshift
        | SourceKind::Iceberg
        | SourceKind::Delta
        | SourceKind::S3
        | SourceKind::Gcs
        | SourceKind::Azure => Err(RegisterError::Other(format!(
            "source kind `{}` is recognized but requires the `lakehouse` build feature",
            def.kind
        ))),
        SourceKind::Postgres | SourceKind::Mysql => Err(RegisterError::Other(format!(
            "source kind `{}` requires the `duckdb-extensions` build feature. \
             (`kind: sqlite` works in-process and is enabled.)",
            def.kind
        ))),
    }
}
