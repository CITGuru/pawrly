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

use datafusion::arrow::datatypes::{DataType, Field, Schema, SchemaRef};
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

/// CSV dialect overrides for a `format: csv` table.
#[derive(Debug, Clone)]
struct CsvOptions {
    has_header: bool,
    delimiter: u8,
    quote: u8,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            has_header: true,
            delimiter: b',',
            quote: b'"',
        }
    }
}

/// On-disk layout for `format: json` files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum JsonLayout {
    /// Sniff the first non-whitespace byte: `[` is an array, else NDJSON.
    #[default]
    Auto,
    /// A single JSON array of objects: `[ {…}, {…} ]`.
    Array,
    /// Newline-delimited JSON (one object per line).
    Ndjson,
}

/// A positional (segment) partition column: its value is the directory name at
/// `index` beneath the glob/dir base, regardless of `key=value` naming.
#[derive(Debug, Clone)]
struct SegmentPartition {
    name: String,
    r#type: DataType,
    index: usize,
}

/// Per-table options parsed from a `kind: file` table declaration.
#[derive(Debug, Clone, Default)]
struct FileTableOptions {
    /// CSV dialect (only consulted for `format: csv`).
    csv: Option<CsvOptions>,
    /// JSON layout (only consulted for `format: json`).
    json_layout: JsonLayout,
    /// Hive partition columns (`key=value` directories) as (name, type).
    partition_cols: Vec<(String, DataType)>,
    /// Positional (segment) partition columns.
    segment_partitions: Vec<SegmentPartition>,
    /// Explicit file schema, overriding inference.
    schema: Option<SchemaRef>,
}

/// Map a YAML type string to an Arrow `DataType` for file columns.
fn file_arrow_type(s: &str) -> DataType {
    match s.to_ascii_lowercase().as_str() {
        "bool" | "boolean" => DataType::Boolean,
        "int" | "int32" => DataType::Int32,
        "bigint" | "int64" | "long" => DataType::Int64,
        "float" | "float32" => DataType::Float32,
        "double" | "float64" => DataType::Float64,
        "date" => DataType::Date32,
        _ => DataType::Utf8,
    }
}

/// Read the first character of a string field as a single byte, e.g. a CSV
/// delimiter. A tab is accepted as `"\t"`.
fn first_byte(v: &serde_json::Value, key: &str) -> Option<u8> {
    let s = v.get(key)?.as_str()?;
    let s = if s == "\\t" { "\t" } else { s };
    s.bytes().next()
}

/// Parse the options block from a table declaration's `config`.
fn parse_table_options(
    source_name: &str,
    tdef: &pawrly_core::TableDef,
) -> Result<FileTableOptions, FileBuildError> {
    let cfg = &tdef.config;
    let mut opts = FileTableOptions::default();

    if let Some(csv) = cfg.get("csv") {
        let d = CsvOptions::default();
        opts.csv = Some(CsvOptions {
            has_header: csv.get("header").and_then(|v| v.as_bool()).unwrap_or(d.has_header),
            delimiter: first_byte(csv, "delimiter").unwrap_or(d.delimiter),
            quote: first_byte(csv, "quote").unwrap_or(d.quote),
        });
    }

    if let Some(j) = cfg.get("json") {
        opts.json_layout = match j.get("format").and_then(|v| v.as_str()) {
            Some(s) if s.eq_ignore_ascii_case("array") => JsonLayout::Array,
            Some(s) if s.eq_ignore_ascii_case("ndjson") || s.eq_ignore_ascii_case("jsonl") => {
                JsonLayout::Ndjson
            }
            _ => JsonLayout::Auto,
        };
    }

    if let Some(parts) = cfg.get("partition_cols").and_then(|v| v.as_array()) {
        for p in parts {
            let name = p
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConfigError::Table {
                    source_name: source_name.to_string(),
                    table: tdef.name.clone(),
                    msg: "partition_cols entry is missing `name`".into(),
                })?;
            let ty = p.get("type").and_then(|v| v.as_str()).unwrap_or("varchar");
            let kind = p.get("kind").and_then(|v| v.as_str()).unwrap_or("hive");
            if kind.eq_ignore_ascii_case("segment") {
                let index = p
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| ConfigError::Table {
                        source_name: source_name.to_string(),
                        table: tdef.name.clone(),
                        msg: format!("segment partition `{name}` requires an `index`"),
                    })?;
                opts.segment_partitions.push(SegmentPartition {
                    name: name.to_string(),
                    r#type: file_arrow_type(ty),
                    index: index as usize,
                });
            } else if kind.eq_ignore_ascii_case("hive") {
                opts.partition_cols
                    .push((name.to_string(), file_arrow_type(ty)));
            } else {
                return Err(ConfigError::Table {
                    source_name: source_name.to_string(),
                    table: tdef.name.clone(),
                    msg: format!("partition kind `{kind}` is not supported (use `hive` or `segment`)"),
                }
                .into());
            }
        }
    }

    if !opts.segment_partitions.is_empty() && !opts.partition_cols.is_empty() {
        return Err(ConfigError::Table {
            source_name: source_name.to_string(),
            table: tdef.name.clone(),
            msg: "a table cannot mix hive and segment partitions".into(),
        }
        .into());
    }

    if let Some(cols) = cfg.get("schema").and_then(|v| v.as_array()) {
        let fields: Vec<Field> = cols
            .iter()
            .filter_map(|c| {
                let name = c.get("name")?.as_str()?;
                let ty = c.get("type").and_then(|v| v.as_str()).unwrap_or("varchar");
                Some(Field::new(name, file_arrow_type(ty), true))
            })
            .collect();
        if !fields.is_empty() {
            opts.schema = Some(Arc::new(Schema::new(fields)));
        }
    }

    Ok(opts)
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
        let options = parse_table_options(&def.name, tdef)?;
        let path = resolve_path(workspace_dir, path_str);
        register_one(
            ctx,
            schema.as_ref(),
            &tdef.name,
            format,
            &path,
            &options,
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
                register_one(
                    ctx,
                    schema.as_ref(),
                    &table_name,
                    format,
                    &path,
                    &FileTableOptions::default(),
                )
                .await?;
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
    options: &FileTableOptions,
) -> Result<(), FileBuildError> {
    use datafusion::datasource::file_format::FileFormat as DfFileFormat;
    use datafusion::datasource::file_format::csv::CsvFormat;
    use datafusion::datasource::file_format::json::JsonFormat;
    use datafusion::datasource::file_format::parquet::ParquetFormat;
    use datafusion::datasource::listing::{
        ListingOptions, ListingTable, ListingTableConfig, ListingTableUrl,
    };

    // Positional (segment) partitions aren't expressible via DataFusion's
    // hive-only listing partitions, so they take a separate path.
    if !options.segment_partitions.is_empty() {
        return register_segment_partitioned(ctx, schema, table_name, format, path, options).await;
    }

    // JSON-array files aren't NDJSON, so DataFusion's listing reader can't parse
    // them. They take a separate in-memory decode path.
    if format == FileFormat::Json && resolve_json_layout(options.json_layout, path)? == JsonLayout::Array
    {
        return register_json_array(schema, table_name, path, options);
    }

    // A single concrete file is canonicalized (resolving symlinks, asserting it
    // exists). A glob or directory is left as an absolute path so DataFusion can
    // list every matching file into one table.
    let path_str = path.to_string_lossy().into_owned();
    let url = if is_glob(&path_str) || path.is_dir() {
        let dir = if path.is_dir() && !path_str.ends_with('/') {
            format!("{path_str}/")
        } else {
            path_str.clone()
        };
        ListingTableUrl::parse(&dir)
            .map_err(|e| FileBuildError::DataFusion(format!("parse listing url: {e}")))?
    } else {
        let abs_path = std::fs::canonicalize(path)
            .map_err(|e| FileBuildError::Io(format!("canonicalize {}: {e}", path.display())))?;
        ListingTableUrl::parse(abs_path.to_string_lossy().as_ref())
            .map_err(|e| FileBuildError::DataFusion(format!("parse listing url: {e}")))?
    };

    let csv = options.csv.clone().unwrap_or_default();
    let (format_impl, default_extension): (Arc<dyn DfFileFormat>, &str) = match format {
        FileFormat::Parquet => (Arc::new(ParquetFormat::default()), ".parquet"),
        FileFormat::Csv => (
            Arc::new(
                CsvFormat::default()
                    .with_has_header(csv.has_header)
                    .with_delimiter(csv.delimiter)
                    .with_quote(csv.quote),
            ),
            ".csv",
        ),
        FileFormat::Json => (Arc::new(JsonFormat::default()), ".json"),
    };

    // For a glob the extension lives in the pattern; for a bare directory we
    // fall back to the format default.
    let actual_extension = if path.is_dir() {
        default_extension.to_string()
    } else {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_else(|| default_extension.to_string())
    };

    let mut listing_options =
        ListingOptions::new(format_impl).with_file_extension(actual_extension);
    if !options.partition_cols.is_empty() {
        listing_options =
            listing_options.with_table_partition_cols(options.partition_cols.clone());
    }

    // An explicit schema overrides inference; otherwise infer from the files.
    let file_schema = match &options.schema {
        Some(s) => s.clone(),
        None => listing_options
            .infer_schema(&ctx.state(), &url)
            .await
            .map_err(|e| FileBuildError::DataFusion(format!("infer schema: {e}")))?,
    };

    let cfg = ListingTableConfig::new(url)
        .with_listing_options(listing_options)
        .with_schema(file_schema);
    let table = ListingTable::try_new(cfg)
        .map_err(|e| FileBuildError::DataFusion(format!("listing table: {e}")))?;
    schema
        .register_table(table_name.to_string(), Arc::new(table))
        .map_err(|e| FileBuildError::DataFusion(format!("register table: {e}")))?;
    Ok(())
}

/// Whether a path string contains shell-glob metacharacters.
fn is_glob(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Resolve a declared JSON layout, sniffing the first file when set to `Auto`.
fn resolve_json_layout(
    declared: JsonLayout,
    path: &std::path::Path,
) -> Result<JsonLayout, FileBuildError> {
    if declared != JsonLayout::Auto {
        return Ok(declared);
    }
    let files = collect_json_files(path)?;
    let Some(first) = files.first() else {
        return Ok(JsonLayout::Ndjson);
    };
    Ok(if first_nonws_is_array(first)? {
        JsonLayout::Array
    } else {
        JsonLayout::Ndjson
    })
}

/// Whether the first non-whitespace byte of a file is `[` (a JSON array).
fn first_nonws_is_array(path: &std::path::Path) -> Result<bool, FileBuildError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)
        .map_err(|e| FileBuildError::Io(format!("open {}: {e}", path.display())))?;
    let mut buf = [0u8; 64];
    let n = f
        .read(&mut buf)
        .map_err(|e| FileBuildError::Io(format!("read {}: {e}", path.display())))?;
    Ok(buf[..n]
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|b| *b == b'['))
}

/// Collect every concrete `.json` file behind a path (a single file, a glob, or
/// a directory walked recursively).
fn collect_json_files(path: &std::path::Path) -> Result<Vec<PathBuf>, FileBuildError> {
    let s = path.to_string_lossy().into_owned();
    if is_glob(&s) {
        let mut v: Vec<PathBuf> = glob::glob(&s)
            .map_err(|e| FileBuildError::Io(format!("glob error: {e}")))?
            .filter_map(Result::ok)
            .filter(|p| p.is_file())
            .collect();
        v.sort();
        if v.is_empty() {
            return Err(FileBuildError::EmptyGlob(s));
        }
        Ok(v)
    } else if path.is_dir() {
        let pattern = format!("{}/**/*.json", s.trim_end_matches('/'));
        let mut v: Vec<PathBuf> = glob::glob(&pattern)
            .map_err(|e| FileBuildError::Io(format!("glob error: {e}")))?
            .filter_map(Result::ok)
            .filter(|p| p.is_file())
            .collect();
        v.sort();
        if v.is_empty() {
            return Err(FileBuildError::EmptyGlob(pattern));
        }
        Ok(v)
    } else {
        Ok(vec![path.to_path_buf()])
    }
}

/// Register a JSON-array file (or glob/dir of them) as an in-memory table.
///
/// DataFusion's JSON reader only handles NDJSON, so we parse each array file,
/// re-emit its elements as NDJSON, and decode that into Arrow batches held in a
/// `MemTable`.
fn register_json_array(
    schema_provider: &dyn SchemaProvider,
    table_name: &str,
    path: &std::path::Path,
    options: &FileTableOptions,
) -> Result<(), FileBuildError> {
    use datafusion::arrow::error::ArrowError;
    use datafusion::arrow::json::ReaderBuilder;
    use datafusion::arrow::json::reader::infer_json_schema_from_iterator;
    use datafusion::arrow::record_batch::RecordBatch;
    use datafusion::datasource::MemTable;
    use std::io::Cursor;

    let files = collect_json_files(path)?;
    let mut elements: Vec<serde_json::Value> = Vec::new();
    for file in &files {
        let bytes = std::fs::read(file)
            .map_err(|e| FileBuildError::Io(format!("read {}: {e}", file.display())))?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| FileBuildError::Io(format!("parse json {}: {e}", file.display())))?;
        match value {
            serde_json::Value::Array(arr) => elements.extend(arr),
            obj @ serde_json::Value::Object(_) => elements.push(obj),
            _ => {
                return Err(FileBuildError::Io(format!(
                    "expected a JSON array or object in {}",
                    file.display()
                )));
            }
        }
    }

    let arrow_schema: SchemaRef = match &options.schema {
        Some(s) => s.clone(),
        None => {
            let inferred =
                infer_json_schema_from_iterator(elements.iter().map(Ok::<&serde_json::Value, ArrowError>))
                    .map_err(|e| FileBuildError::DataFusion(format!("infer json schema: {e}")))?;
            Arc::new(inferred)
        }
    };

    // Re-emit the elements as NDJSON and decode into record batches.
    let mut ndjson: Vec<u8> = Vec::new();
    for el in &elements {
        serde_json::to_writer(&mut ndjson, el)
            .map_err(|e| FileBuildError::Io(format!("serialize json row: {e}")))?;
        ndjson.push(b'\n');
    }
    let reader = ReaderBuilder::new(arrow_schema.clone())
        .build(Cursor::new(ndjson))
        .map_err(|e| FileBuildError::DataFusion(format!("json reader: {e}")))?;
    let batches: Vec<RecordBatch> = reader
        .collect::<Result<_, _>>()
        .map_err(|e| FileBuildError::DataFusion(format!("decode json: {e}")))?;

    let table = MemTable::try_new(arrow_schema, vec![batches])
        .map_err(|e| FileBuildError::DataFusion(format!("mem table: {e}")))?;
    schema_provider
        .register_table(table_name.to_string(), Arc::new(table))
        .map_err(|e| FileBuildError::DataFusion(format!("register table: {e}")))?;
    Ok(())
}

/// The literal (non-glob) directory prefix of a path — the base against which
/// segment-partition indices are measured.
fn glob_base(path: &std::path::Path) -> PathBuf {
    let mut base = PathBuf::new();
    for comp in path.components() {
        let s = comp.as_os_str().to_string_lossy();
        if s.contains(['*', '?', '[']) {
            break;
        }
        base.push(comp);
    }
    base
}

/// Collect concrete files behind a path (single file, glob, or directory walked
/// recursively for any of `exts`).
fn collect_files(path: &std::path::Path, exts: &[&str]) -> Result<Vec<PathBuf>, FileBuildError> {
    let s = path.to_string_lossy().into_owned();
    let run = |pattern: &str| -> Result<Vec<PathBuf>, FileBuildError> {
        let mut v: Vec<PathBuf> = glob::glob(pattern)
            .map_err(|e| FileBuildError::Io(format!("glob error: {e}")))?
            .filter_map(Result::ok)
            .filter(|p| p.is_file())
            .collect();
        v.sort();
        Ok(v)
    };
    let files = if is_glob(&s) {
        run(&s)?
    } else if path.is_dir() {
        let mut v = Vec::new();
        for ext in exts {
            v.extend(run(&format!("{}/**/*.{ext}", s.trim_end_matches('/')))?);
        }
        v.sort();
        v
    } else {
        return Ok(vec![path.to_path_buf()]);
    };
    if files.is_empty() {
        return Err(FileBuildError::EmptyGlob(s));
    }
    Ok(files)
}

/// File extensions associated with a format, for directory walks.
fn format_extensions(format: FileFormat) -> &'static [&'static str] {
    match format {
        FileFormat::Parquet => &["parquet"],
        FileFormat::Csv => &["csv"],
        FileFormat::Json => &["json", "jsonl", "ndjson"],
    }
}

/// Build a constant Arrow array of `value` (typed per `dtype`), length `len`.
fn constant_array(dtype: &DataType, value: &str, len: usize) -> arrow_array::ArrayRef {
    use arrow_array::{
        BooleanArray, Date32Array, Float64Array, Int32Array, Int64Array, StringArray,
    };
    match dtype {
        DataType::Int64 => Arc::new(Int64Array::from(vec![value.parse::<i64>().ok(); len])),
        DataType::Int32 => Arc::new(Int32Array::from(vec![value.parse::<i32>().ok(); len])),
        DataType::Float64 => Arc::new(Float64Array::from(vec![value.parse::<f64>().ok(); len])),
        DataType::Boolean => Arc::new(BooleanArray::from(vec![value.parse::<bool>().ok(); len])),
        DataType::Date32 => Arc::new(Date32Array::from(vec![parse_date32(value); len])),
        _ => Arc::new(StringArray::from(vec![value.to_string(); len])),
    }
}

/// Parse a `YYYY-MM-DD` string into days since the Unix epoch (Arrow `Date32`).
fn parse_date32(s: &str) -> Option<i32> {
    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?;
    i32::try_from((date - epoch).num_days()).ok()
}

/// Register a segment-partitioned table: read each matched file, append the
/// positional partition columns derived from its path, and hold the result in a
/// `MemTable`.
///
/// Unlike hive partitions (served by a streaming `ListingTable`), segment
/// partitions are materialized in memory, so they do not get partition pruning.
async fn register_segment_partitioned(
    ctx: &SessionContext,
    schema_provider: &dyn SchemaProvider,
    table_name: &str,
    format: FileFormat,
    path: &std::path::Path,
    options: &FileTableOptions,
) -> Result<(), FileBuildError> {
    use arrow_array::RecordBatch;
    use datafusion::datasource::MemTable;
    use datafusion::prelude::{CsvReadOptions, JsonReadOptions, ParquetReadOptions};

    let base = glob_base(path);
    let files = collect_files(path, format_extensions(format))?;
    let csv = options.csv.clone().unwrap_or_default();

    let part_fields: Vec<Field> = options
        .segment_partitions
        .iter()
        .map(|p| Field::new(&p.name, p.r#type.clone(), true))
        .collect();

    let mut out_batches: Vec<RecordBatch> = Vec::new();
    let mut out_schema: Option<SchemaRef> = None;

    for file in &files {
        // Derive each partition value from the directory segment at its index.
        let rel = file.strip_prefix(&base).unwrap_or(file);
        let dir_segments: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        // The last component is the file name itself.
        let dir_segments = &dir_segments[..dir_segments.len().saturating_sub(1)];
        let values: Vec<String> = options
            .segment_partitions
            .iter()
            .map(|p| dir_segments.get(p.index).cloned().unwrap_or_default())
            .collect();

        let path_str = file.to_string_lossy().into_owned();
        let df = match format {
            FileFormat::Parquet => ctx
                .read_parquet(path_str, ParquetReadOptions::default())
                .await
                .map_err(|e| FileBuildError::DataFusion(format!("read parquet: {e}")))?,
            FileFormat::Csv => ctx
                .read_csv(
                    path_str,
                    CsvReadOptions::new()
                        .has_header(csv.has_header)
                        .delimiter(csv.delimiter),
                )
                .await
                .map_err(|e| FileBuildError::DataFusion(format!("read csv: {e}")))?,
            FileFormat::Json => ctx
                .read_json(path_str, JsonReadOptions::default())
                .await
                .map_err(|e| FileBuildError::DataFusion(format!("read json: {e}")))?,
        };
        let batches = df
            .collect()
            .await
            .map_err(|e| FileBuildError::DataFusion(format!("collect {}: {e}", file.display())))?;

        for batch in batches {
            let n = batch.num_rows();
            let mut columns = batch.columns().to_vec();
            for (p, value) in options.segment_partitions.iter().zip(&values) {
                columns.push(constant_array(&p.r#type, value, n));
            }
            let mut fields: Vec<Field> = batch
                .schema()
                .fields()
                .iter()
                .map(|f| f.as_ref().clone())
                .collect();
            fields.extend(part_fields.iter().cloned());
            let schema: SchemaRef = Arc::new(Schema::new(fields));
            out_schema.get_or_insert_with(|| schema.clone());
            let augmented = RecordBatch::try_new(schema, columns)
                .map_err(|e| FileBuildError::DataFusion(format!("augment batch: {e}")))?;
            out_batches.push(augmented);
        }
    }

    let schema = match out_schema {
        Some(s) => s,
        None => {
            return Err(FileBuildError::DataFusion(
                "segment-partitioned table matched no rows".into(),
            ));
        }
    };
    let table = MemTable::try_new(schema, vec![out_batches])
        .map_err(|e| FileBuildError::DataFusion(format!("mem table: {e}")))?;
    schema_provider
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
