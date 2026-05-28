//! File source: registers parquet / csv / json files (or globs) as
//! DataFusion tables under the source's schema.
//!
//! YAML shape:
//!
//! ```yaml
//! - name: data
//!   kind: file
//!   config:
//!     path: ./data/*.parquet     # optional top-level glob
//!   tables:                      # optional explicit overrides
//!     - name: orders
//!       path: ./data/orders.parquet
//!       format: parquet          # auto-detected if omitted
//!     - name: customers
//!       path: ./data/customers.csv
//!       format: csv
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use datafusion::catalog::{CatalogProvider, MemorySchemaProvider, SchemaProvider};
use datafusion::execution::context::SessionContext;
use pawrly_core::{ConfigError, SourceDef};

/// Per-table summary surfaced to the engine for `list_tables`.
#[derive(Debug, Clone)]
pub struct FileSummary {
    pub name: String,
    pub description: Option<String>,
    pub format: FileFormat,
    pub path: PathBuf,
}

/// Outcome of `register_file_source`.
#[derive(Debug, Default)]
pub struct FileSourceReport {
    pub table_count: u64,
    pub tables: Vec<FileSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    Parquet,
    Csv,
    Json,
}

#[derive(Debug, thiserror::Error)]
pub enum FileBuildError {
    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    #[error("io: {0}")]
    Io(String),

    #[error("datafusion: {0}")]
    DataFusion(String),

    #[error("unsupported file format `{0}` (supported: parquet, csv, json)")]
    UnsupportedFormat(String),

    #[error("could not infer format from path `{0}` (specify `format:`)")]
    UnknownExtension(PathBuf),

    #[error("no files matched glob `{0}`")]
    EmptyGlob(String),
}

/// Lightweight validation of a `kind: file` source's config.
pub fn validate_file_def(def: &SourceDef) -> Result<(), ConfigError> {
    let top_path = def.config.get("path").and_then(|v| v.as_str());
    if top_path.is_none() && def.tables.is_empty() {
        return Err(ConfigError::Source(
            def.name.clone(),
            "kind: file requires either config.path or at least one tables entry".into(),
        ));
    }
    Ok(())
}

/// Register all tables for a `kind: file` source on the given catalog.
pub async fn register_file_source(
    def: &SourceDef,
    ctx: &SessionContext,
    catalog: &dyn CatalogProvider,
    workspace_dir: &std::path::Path,
) -> Result<FileSourceReport, FileBuildError> {
    let schema = ensure_schema(catalog, &def.name)?;

    let mut summaries = Vec::new();

    // Explicit per-table declarations come first.
    for tdef in &def.tables {
        let path_str = tdef
            .config
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ConfigError::Table {
                source_name: def.name.clone(),
                table: tdef.name.clone(),
                msg: "table is missing required `path:`".into(),
            })?;
        let format = match tdef.config.get("format").and_then(|v| v.as_str()) {
            Some(s) => parse_format(s)?,
            None => infer_format_from_path(path_str)?,
        };
        let path = resolve_path(workspace_dir, path_str);
        register_one(
            ctx,
            schema.as_ref(),
            &tdef.name,
            format,
            &path,
            tdef.description.clone(),
        )
        .await?;
        summaries.push(FileSummary {
            name: tdef.name.clone(),
            description: tdef.description.clone(),
            format,
            path,
        });
    }

    // Top-level glob, if no explicit tables were given.
    if def.tables.is_empty() {
        if let Some(glob_pat) = def.config.get("path").and_then(|v| v.as_str()) {
            let glob_resolved = resolve_glob(workspace_dir, glob_pat);
            let entries: Vec<_> = glob::glob(&glob_resolved)
                .map_err(|e| FileBuildError::Io(format!("glob error: {e}")))?
                .filter_map(Result::ok)
                .filter(|p| p.is_file())
                .collect();
            if entries.is_empty() {
                return Err(FileBuildError::EmptyGlob(glob_pat.to_string()));
            }
            for path in entries {
                let format = infer_format_from_path(path.to_string_lossy().as_ref())?;
                let table_name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| "unnamed".to_string());
                register_one(ctx, schema.as_ref(), &table_name, format, &path, None).await?;
                summaries.push(FileSummary {
                    name: table_name,
                    description: None,
                    format,
                    path,
                });
            }
        }
    }

    Ok(FileSourceReport {
        table_count: summaries.len() as u64,
        tables: summaries,
    })
}

fn ensure_schema(
    catalog: &dyn CatalogProvider,
    name: &str,
) -> Result<Arc<dyn SchemaProvider>, FileBuildError> {
    if let Some(s) = catalog.schema(name) {
        return Ok(s);
    }
    let s: Arc<dyn SchemaProvider> = Arc::new(MemorySchemaProvider::new());
    // CatalogProvider::register_schema returns Result<Option<Arc<dyn SchemaProvider>>, ...>
    // when supported. MemoryCatalogProvider supports it.
    if let Some(memory_catalog) = catalog
        .as_any()
        .downcast_ref::<datafusion::catalog::MemoryCatalogProvider>()
    {
        let _ = memory_catalog
            .register_schema(name, s.clone())
            .map_err(|e| FileBuildError::DataFusion(e.to_string()))?;
        Ok(s)
    } else {
        Err(FileBuildError::DataFusion(
            "catalog is not a MemoryCatalogProvider; schema registration unsupported".into(),
        ))
    }
}

async fn register_one(
    ctx: &SessionContext,
    schema: &dyn SchemaProvider,
    table_name: &str,
    format: FileFormat,
    path: &std::path::Path,
    _description: Option<String>,
) -> Result<(), FileBuildError> {
    use datafusion::datasource::file_format::FileFormat as DfFileFormat;
    use datafusion::datasource::file_format::csv::CsvFormat;
    use datafusion::datasource::file_format::json::JsonFormat;
    use datafusion::datasource::file_format::parquet::ParquetFormat;
    use datafusion::datasource::listing::{
        ListingOptions, ListingTable, ListingTableConfig, ListingTableUrl,
    };

    // Canonicalize so DataFusion's URL parser knows the file is local + absolute.
    let abs_path = std::fs::canonicalize(path)
        .map_err(|e| FileBuildError::Io(format!("canonicalize {}: {e}", path.display())))?;
    let url = ListingTableUrl::parse(abs_path.to_string_lossy().as_ref())
        .map_err(|e| FileBuildError::DataFusion(format!("parse listing url: {e}")))?;

    let (format_impl, default_extension): (Arc<dyn DfFileFormat>, &str) = match format {
        FileFormat::Parquet => (Arc::new(ParquetFormat::default()), ".parquet"),
        FileFormat::Csv => (Arc::new(CsvFormat::default().with_has_header(true)), ".csv"),
        FileFormat::Json => (Arc::new(JsonFormat::default()), ".json"),
    };

    let actual_extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_else(|| default_extension.to_string());

    let options = ListingOptions::new(format_impl).with_file_extension(actual_extension);
    let resolved_schema = options
        .infer_schema(&ctx.state(), &url)
        .await
        .map_err(|e| FileBuildError::DataFusion(format!("infer schema: {e}")))?;
    let cfg = ListingTableConfig::new(url)
        .with_listing_options(options)
        .with_schema(resolved_schema);
    let table = ListingTable::try_new(cfg)
        .map_err(|e| FileBuildError::DataFusion(format!("listing table: {e}")))?;
    schema
        .register_table(table_name.to_string(), Arc::new(table))
        .map_err(|e| FileBuildError::DataFusion(format!("register table: {e}")))?;
    Ok(())
}

fn parse_format(s: &str) -> Result<FileFormat, FileBuildError> {
    match s.to_ascii_lowercase().as_str() {
        "parquet" => Ok(FileFormat::Parquet),
        "csv" => Ok(FileFormat::Csv),
        "json" | "jsonl" | "ndjson" => Ok(FileFormat::Json),
        other => Err(FileBuildError::UnsupportedFormat(other.to_string())),
    }
}

fn infer_format_from_path(s: &str) -> Result<FileFormat, FileBuildError> {
    let lower = s.to_ascii_lowercase();
    if lower.ends_with(".parquet") {
        Ok(FileFormat::Parquet)
    } else if lower.ends_with(".csv") {
        Ok(FileFormat::Csv)
    } else if lower.ends_with(".json") || lower.ends_with(".jsonl") || lower.ends_with(".ndjson") {
        Ok(FileFormat::Json)
    } else {
        Err(FileBuildError::UnknownExtension(PathBuf::from(s)))
    }
}

fn resolve_path(workspace_dir: &std::path::Path, p: &str) -> PathBuf {
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_dir.join(path)
    }
}

fn resolve_glob(workspace_dir: &std::path::Path, p: &str) -> String {
    if std::path::Path::new(p).is_absolute() {
        p.to_string()
    } else {
        workspace_dir.join(p).to_string_lossy().into_owned()
    }
}
