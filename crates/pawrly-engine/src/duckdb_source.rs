//! DuckDB-backed sources: postgres / mysql / snowflake / duckdb (ATTACH
//! databases), iceberg / delta (scan functions), ducklake (DuckLake catalog),
//! and object storage / http(s) for remote `file` sources
//! (`read_parquet`/csv/json via httpfs).
//!
//! Every kind converges on a single piece of machinery: each table ends up as
//! a DuckDB SQL relation — a FROM-clause snippet — that one
//! [`DuckDbTableProvider`] scans. Schema inference is free because DuckDB is
//! Arrow-native: we run `SELECT * FROM <relation> WHERE 1=0` and take the
//! schema off the empty result. Predicate / projection / limit push-down is
//! done by rewriting them into the SQL the provider runs.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::{Array, StringArray};
use arrow_schema::SchemaRef;
use async_trait::async_trait;
use base64::Engine as _;
use datafusion::catalog::{
    CatalogProvider, MemoryCatalogProvider, MemorySchemaProvider, SchemaProvider, Session,
};
use datafusion::common::DataFusionError;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::execution::TaskContext;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::physical_plan::SendableRecordBatchStream;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::streaming::{PartitionStream, StreamingTableExec};
use parking_lot::Mutex;
use pawrly_core::{SourceDef, SourceKind, StorageScheme, origin_prefix};
use pawrly_sources_http::AuthSpec;
use secrecy::ExposeSecret as _;
use serde_json::Value as JsonValue;

use crate::duckdb_pool::DuckDbPool;
use crate::registry::{RegisterError, RegisterReport, TableSummary};

/// A DuckDB-backed `TableProvider`. The `relation` is any FROM-clause snippet
/// DuckDB understands: an attached `"db"."schema"."tbl"`, an
/// `iceberg_scan('…')` / `delta_scan('…')`, or a `read_parquet('s3://… glob')`.
#[derive(Debug)]
pub struct DuckDbTableProvider {
    pool: Arc<DuckDbPool>,
    relation: String,
    schema: SchemaRef,
}

impl DuckDbTableProvider {
    /// Build a provider for `relation`, inferring its schema via a zero-row
    /// probe (`WHERE 1=0`). DuckDB is Arrow-native, so no manual type mapping.
    pub async fn try_new(pool: Arc<DuckDbPool>, relation: String) -> Result<Self, DataFusionError> {
        // Probe for the schema with a zero-row query. We use the streaming path
        // because DuckDB's eager `fetch_arrow` yields no batch at all for an
        // empty result, whereas the stream carries the schema regardless of row
        // count (DuckDB is Arrow-native, so no manual type mapping).
        let probe = format!("SELECT * FROM {relation} WHERE 1=0");
        let stream = pool
            .fetch_arrow_stream(&probe)
            .await
            .map_err(|e| DataFusionError::External(Box::new(e)))?;
        let schema = stream.schema();
        Ok(Self {
            pool,
            relation,
            schema,
        })
    }

    /// Build the projected `SELECT … FROM <relation> [WHERE …] [LIMIT n]`.
    fn build_sql(
        &self,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> String {
        let cols = match projection {
            // An empty projection (e.g. `COUNT(*)`) needs rows but no columns.
            // SQL can't select zero columns, so select a constant and drop it in
            // the stream (see `DuckDbScanStream::execute`).
            Some(p) if p.is_empty() => "1".to_string(),
            Some(p) => p
                .iter()
                .map(|i| format!("\"{}\"", self.schema.field(*i).name()))
                .collect::<Vec<_>>()
                .join(", "),
            None => "*".to_string(),
        };
        let mut sql = format!("SELECT {cols} FROM {}", self.relation);
        let clauses: Vec<String> = filters
            .iter()
            .filter_map(extract_eq_literal)
            .map(|(col, lit)| format!("\"{col}\" = {lit}"))
            .collect();
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        sql
    }
}

impl pawrly_core::DynamicFilterCapable for DuckDbTableProvider {
    fn dynamic_filter_columns(&self) -> Vec<String> {
        self.schema
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }
}

#[async_trait]
impl TableProvider for DuckDbTableProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|f| {
                if extract_eq_literal(f).is_some() {
                    TableProviderFilterPushDown::Exact
                } else {
                    TableProviderFilterPushDown::Unsupported
                }
            })
            .collect())
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let sql = self.build_sql(projection, filters, limit);
        let projected_schema = match projection {
            Some(p) => Arc::new(self.schema.project(p)?),
            None => self.schema.clone(),
        };
        let stream = DuckDbScanStream {
            pool: self.pool.clone(),
            sql,
            schema: projected_schema.clone(),
        };
        // Projection + filters + limit are already baked into the SQL, so the
        // exec runs the rewritten query verbatim (projection/limit = None here).
        let exec = StreamingTableExec::try_new(
            projected_schema,
            vec![Arc::new(stream)],
            None,
            Vec::new(),
            false,
            None,
        )?;
        Ok(Arc::new(exec))
    }
}

/// One partition that lazily runs `sql` against the pool when executed.
#[derive(Debug)]
struct DuckDbScanStream {
    pool: Arc<DuckDbPool>,
    sql: String,
    schema: SchemaRef,
}

impl PartitionStream for DuckDbScanStream {
    fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    fn execute(&self, _ctx: Arc<TaskContext>) -> SendableRecordBatchStream {
        use futures::stream::{StreamExt, TryStreamExt};
        let pool = self.pool.clone();
        let sql = self.sql.clone();
        let schema = self.schema.clone();
        // An empty output schema (`COUNT(*)` etc.) means the SQL selected a
        // throwaway constant column (`SELECT 1`); rewrite each batch to zero
        // columns, preserving only the row count.
        let empty = schema.fields().is_empty();
        let out = schema.clone();
        // `fetch_arrow_stream` is async and may fail; wrap it in a one-shot
        // stream and `try_flatten` so the partition stream yields RecordBatch
        // results lazily. The future's `Ok` is a `SendableRecordBatchStream`
        // whose items are already `Result<RecordBatch, DataFusionError>`, so
        // flattening composes cleanly: an attach/extension error surfaces as
        // the stream's first (and only) error item.
        let inner = futures::stream::once(async move {
            pool.fetch_arrow_stream(&sql)
                .await
                .map_err(|e| DataFusionError::External(Box::new(e)))
        })
        .try_flatten()
        .map(move |res| {
            res.and_then(|batch| {
                if empty {
                    let opts = arrow_array::RecordBatchOptions::new()
                        .with_row_count(Some(batch.num_rows()));
                    arrow_array::RecordBatch::try_new_with_options(out.clone(), vec![], &opts)
                        .map_err(DataFusionError::from)
                } else {
                    Ok(batch)
                }
            })
        });
        Box::pin(RecordBatchStreamAdapter::new(schema, inner))
    }
}

/// Lazy schema provider over an ATTACHed DuckDB database. Table names come from
/// `information_schema`; providers are built (and cached) on first access.
#[derive(Debug)]
struct DuckDbSchemaProvider {
    pool: Arc<DuckDbPool>,
    db_name: String,
    cache: Mutex<HashMap<String, Arc<dyn TableProvider>>>,
}

impl DuckDbSchemaProvider {
    fn new(pool: Arc<DuckDbPool>, db_name: String) -> Self {
        Self {
            pool,
            db_name,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl SchemaProvider for DuckDbSchemaProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn table_names(&self) -> Vec<String> {
        let db = self.db_name.replace('\'', "''");
        let sql = format!(
            "SELECT table_name FROM information_schema.tables WHERE table_catalog = '{db}'"
        );
        // SchemaProvider::table_names is sync; run the async fetch on the
        // current runtime via a blocking handle.
        let pool = self.pool.clone();
        let res = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move { pool.fetch_arrow(&sql).await })
        });
        match res {
            Ok(batches) => string_column(&batches),
            Err(_) => Vec::new(),
        }
    }

    async fn table(&self, name: &str) -> Result<Option<Arc<dyn TableProvider>>, DataFusionError> {
        if let Some(p) = self.cache.lock().get(name) {
            return Ok(Some(p.clone()));
        }
        // Find the table's schema so the qualified relation is correct across
        // engines (postgres `public`, mysql DB name, snowflake schema, …).
        let db = self.db_name.replace('\'', "''");
        let name_esc = name.replace('\'', "''");
        let sql = format!(
            "SELECT table_schema FROM information_schema.tables \
             WHERE table_catalog = '{db}' AND table_name = '{name_esc}' LIMIT 1"
        );
        let batches = self
            .pool
            .fetch_arrow(&sql)
            .await
            .map_err(|e| DataFusionError::External(Box::new(e)))?;
        let table_schema = string_column(&batches).into_iter().next();
        let Some(table_schema) = table_schema else {
            return Ok(None);
        };
        let relation = format!("\"{}\".\"{}\".\"{}\"", self.db_name, table_schema, name);
        let provider: Arc<dyn TableProvider> =
            Arc::new(DuckDbTableProvider::try_new(self.pool.clone(), relation).await?);
        self.cache.lock().insert(name.to_string(), provider.clone());
        Ok(Some(provider))
    }

    fn table_exist(&self, name: &str) -> bool {
        if self.cache.lock().contains_key(name) {
            return true;
        }
        self.table_names().iter().any(|n| n == name)
    }
}

/// Extract the first string column out of a set of batches.
fn string_column(batches: &[arrow_array::RecordBatch]) -> Vec<String> {
    let mut out = Vec::new();
    for batch in batches {
        if batch.num_columns() == 0 {
            continue;
        }
        if let Some(arr) = batch.column(0).as_any().downcast_ref::<StringArray>() {
            for i in 0..arr.len() {
                if arr.is_valid(i) {
                    out.push(arr.value(i).to_string());
                }
            }
        }
    }
    out
}

/// Register a DuckDB-backed source. Dispatches on `def.kind` into one of four
/// strategies: ATTACH databases, lakehouse scan functions, DuckLake catalogs,
/// or object stores (remote `file`).
pub async fn register_duckdb_source(
    def: &SourceDef,
    pool: &Arc<DuckDbPool>,
    catalog: &dyn CatalogProvider,
    workspace_dir: &std::path::Path,
) -> Result<RegisterReport, RegisterError> {
    match def.kind {
        SourceKind::Postgres | SourceKind::Mysql | SourceKind::Snowflake | SourceKind::Duckdb => {
            register_attach(def, pool, catalog, workspace_dir).await
        }
        SourceKind::Iceberg | SourceKind::Delta => {
            register_scan(def, pool, catalog, workspace_dir).await
        }
        SourceKind::Ducklake => register_ducklake(def, pool, catalog, workspace_dir).await,
        SourceKind::File => register_object_store(def, pool, catalog, workspace_dir).await,
        other => Err(RegisterError::Other(format!(
            "register_duckdb_source called for unsupported kind `{other}`"
        ))),
    }
}

/// Resolve a local path against the workspace directory. Remote URLs (`s3://…`)
/// and absolute paths are returned unchanged; relative local paths are joined to
/// `workspace_dir` so they resolve against the config file, not the CWD.
fn resolve_local(workspace_dir: &std::path::Path, raw: &str) -> String {
    if raw.contains("://") || std::path::Path::new(raw).is_absolute() {
        raw.to_string()
    } else {
        workspace_dir.join(raw).to_string_lossy().into_owned()
    }
}

/// ATTACH a foreign database (postgres / mysql / snowflake) or a local DuckDB
/// database file (`duckdb`), and expose its tables lazily via a
/// [`DuckDbSchemaProvider`].
async fn register_attach(
    def: &SourceDef,
    pool: &Arc<DuckDbPool>,
    catalog: &dyn CatalogProvider,
    workspace_dir: &std::path::Path,
) -> Result<RegisterReport, RegisterError> {
    // `ext` is the DuckDB extension to load (None for native .duckdb attach);
    // `ty` is the ATTACH `TYPE` (None for a .duckdb file, which DuckDB infers).
    let (ext, ty, conn): (Option<&str>, Option<&str>, String) = match def.kind {
        SourceKind::Postgres => (
            Some("postgres"),
            Some("postgres"),
            postgres_mysql_conn(&def.config, "postgresql")?,
        ),
        SourceKind::Mysql => (
            Some("mysql"),
            Some("mysql"),
            postgres_mysql_conn(&def.config, "mysql")?,
        ),
        SourceKind::Snowflake => (
            Some("snowflake"),
            Some("snowflake"),
            snowflake_conn(&def.config)?,
        ),
        SourceKind::Duckdb => (
            None,
            None,
            resolve_local(workspace_dir, &duckdb_file_path(&def.config)?),
        ),
        other => {
            return Err(RegisterError::Other(format!(
                "register_attach called for `{other}`"
            )));
        }
    };

    // Load the extension. Snowflake's is a community extension, not bundled, so
    // it is installed explicitly (skipping INSTALL offline). A `.duckdb` file
    // needs no extension.
    if def.kind == SourceKind::Snowflake {
        let sql = if pool.offline() {
            "LOAD snowflake;".to_string()
        } else {
            "INSTALL snowflake FROM community; LOAD snowflake;".to_string()
        };
        pool.execute(&sql)
            .await
            .map_err(|e| RegisterError::Other(e.to_string()))?;
    } else if let Some(ext) = ext {
        pool.ensure_extension(ext)
            .await
            .map_err(|e| RegisterError::Other(e.to_string()))?;
    }

    let conn_esc = conn.replace('\'', "''");
    let name = &def.name;
    // Snowflake does not accept READ_ONLY; everything else attaches read-only.
    // A `.duckdb` file attaches with no TYPE clause (DuckDB infers it).
    let attach = match (ty, def.kind == SourceKind::Snowflake) {
        (Some(ty), true) => format!("ATTACH '{conn_esc}' AS \"{name}\" (TYPE {ty})"),
        (Some(ty), false) => format!("ATTACH '{conn_esc}' AS \"{name}\" (TYPE {ty}, READ_ONLY)"),
        (None, _) => format!("ATTACH '{conn_esc}' AS \"{name}\" (READ_ONLY)"),
    };
    pool.execute(&attach)
        .await
        .map_err(|e| RegisterError::Other(e.to_string()))?;

    let schema: Arc<dyn SchemaProvider> =
        Arc::new(DuckDbSchemaProvider::new(pool.clone(), def.name.clone()));
    register_schema(catalog, &def.name, schema)?;

    // Eagerly enumerate the attached catalog so its tables show in `schema` and
    // MCP discovery; per-table schema inference stays lazy (at query time).
    let tables = enumerate_catalog_tables(pool, &def.name).await;
    Ok(RegisterReport {
        table_count: tables.len() as u64,
        tables,
        function_handle: Default::default(),
    })
}

/// Iceberg / delta: each declared table maps to a scan function over a path.
async fn register_scan(
    def: &SourceDef,
    pool: &Arc<DuckDbPool>,
    catalog: &dyn CatalogProvider,
    workspace_dir: &std::path::Path,
) -> Result<RegisterReport, RegisterError> {
    let ext = match def.kind {
        SourceKind::Iceberg => "iceberg",
        SourceKind::Delta => "delta",
        other => {
            return Err(RegisterError::Other(format!(
                "register_scan called for `{other}`"
            )));
        }
    };
    pool.ensure_extension(ext)
        .await
        .map_err(|e| RegisterError::Other(e.to_string()))?;

    let schema = ensure_memory_schema(catalog, &def.name)?;
    let mut summaries = Vec::with_capacity(def.tables.len());
    for table in &def.tables {
        let loc = table_location(&table.config).ok_or_else(|| {
            RegisterError::Other(format!(
                "table `{}` is missing `path`/`location`",
                table.name
            ))
        })?;
        let loc_esc = resolve_local(workspace_dir, &loc).replace('\'', "''");
        let relation = match def.kind {
            SourceKind::Iceberg => format!("iceberg_scan('{loc_esc}')"),
            _ => format!("delta_scan('{loc_esc}')"),
        };
        let provider = DuckDbTableProvider::try_new(pool.clone(), relation)
            .await
            .map_err(|e| RegisterError::Other(e.to_string()))?;
        schema
            .register_table(table.name.clone(), Arc::new(provider))
            .map_err(|e| RegisterError::Other(e.to_string()))?;
        summaries.push(TableSummary {
            name: table.name.clone(),
            description: table.description.clone(),
            wiki: None,
            required_filters: Vec::new(),
        });
    }
    Ok(RegisterReport {
        table_count: summaries.len() as u64,
        tables: summaries,
        function_handle: Default::default(),
    })
}

/// DuckLake catalog: ATTACH `'ducklake:<catalog>'` (optionally with a
/// `DATA_PATH`), then expose its tables lazily via a [`DuckDbSchemaProvider`].
async fn register_ducklake(
    def: &SourceDef,
    pool: &Arc<DuckDbPool>,
    catalog: &dyn CatalogProvider,
    workspace_dir: &std::path::Path,
) -> Result<RegisterReport, RegisterError> {
    pool.ensure_extension("ducklake")
        .await
        .map_err(|e| RegisterError::Other(e.to_string()))?;

    let catalog_uri = def
        .config
        .get("catalog")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            RegisterError::Other("`kind: ducklake` requires `config.catalog`".to_string())
        })?;
    // A remote `data_path` needs object-store credentials (via httpfs + secret).
    if def.config.get("storage").is_some() {
        pool.ensure_extension("httpfs")
            .await
            .map_err(|e| RegisterError::Other(e.to_string()))?;
        for secret in build_secret_sql(def).map_err(RegisterError::Other)? {
            pool.execute(&secret)
                .await
                .map_err(|e| RegisterError::Other(e.to_string()))?;
        }
    }

    let catalog_esc = resolve_local(workspace_dir, catalog_uri).replace('\'', "''");
    let name = &def.name;
    // READ_ONLY matches pawrly's read-only model (like the other ATTACHes) and
    // is required to read a remote catalog over httpfs (e.g. an http(s) `.ducklake`).
    let attach = match def.config.get("data_path").and_then(|v| v.as_str()) {
        Some(data_path) => {
            let dp_esc = resolve_local(workspace_dir, data_path).replace('\'', "''");
            format!(
                "ATTACH 'ducklake:{catalog_esc}' AS \"{name}\" (DATA_PATH '{dp_esc}', READ_ONLY)"
            )
        }
        None => format!("ATTACH 'ducklake:{catalog_esc}' AS \"{name}\" (READ_ONLY)"),
    };
    pool.execute(&attach)
        .await
        .map_err(|e| RegisterError::Other(e.to_string()))?;

    let schema: Arc<dyn SchemaProvider> =
        Arc::new(DuckDbSchemaProvider::new(pool.clone(), def.name.clone()));
    register_schema(catalog, &def.name, schema)?;
    let tables = enumerate_catalog_tables(pool, &def.name).await;
    Ok(RegisterReport {
        table_count: tables.len() as u64,
        tables,
        function_handle: Default::default(),
    })
}

/// Enumerate the tables of an attached DuckDB catalog (postgres / mysql /
/// snowflake / duckdb / ducklake) so they appear in `pawrly schema` and MCP
/// discovery. Best-effort: on error, return empty and rely on lazy per-table
/// resolution at query time (the catalog still works, it just won't list).
async fn enumerate_catalog_tables(pool: &Arc<DuckDbPool>, catalog: &str) -> Vec<TableSummary> {
    let cat = catalog.replace('\'', "''");
    let sql = format!(
        "SELECT DISTINCT table_name FROM information_schema.tables \
         WHERE table_catalog = '{cat}' ORDER BY table_name"
    );
    match pool.fetch_arrow(&sql).await {
        Ok(batches) => string_column(&batches)
            .into_iter()
            .map(|name| TableSummary {
                name,
                description: None,
                wiki: None,
                required_filters: Vec::new(),
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Remote `file` over object storage or http(s): synthesize the storage
/// secret(s), then scan each declared table's URL with the reader inferred from
/// its extension (or its explicit `format`).
async fn register_object_store(
    def: &SourceDef,
    pool: &Arc<DuckDbPool>,
    catalog: &dyn CatalogProvider,
    workspace_dir: &std::path::Path,
) -> Result<RegisterReport, RegisterError> {
    pool.ensure_extension("httpfs")
        .await
        .map_err(|e| RegisterError::Other(e.to_string()))?;

    for secret in build_secret_sql(def).map_err(RegisterError::Other)? {
        pool.execute(&secret)
            .await
            .map_err(|e| RegisterError::Other(e.to_string()))?;
    }

    let schema = ensure_memory_schema(catalog, &def.name)?;
    let mut summaries = Vec::with_capacity(def.tables.len());
    for table in &def.tables {
        let url = table_location(&table.config)
            .map(|loc| resolve_local(workspace_dir, &loc))
            .ok_or_else(|| {
                RegisterError::Other(format!(
                    "table `{}` is missing `path`/`location`",
                    table.name
                ))
            })?;
        // httpfs cannot glob plain http(s) URLs (no directory listing). Reject a
        // glob in the path early — `?`/`&` are query syntax, not globs.
        if StorageScheme::classify(&url) == StorageScheme::Http {
            let path_only = url.split('?').next().unwrap_or(&url);
            if path_only.contains('*') || path_only.contains('[') {
                return Err(RegisterError::Other(format!(
                    "table `{}`: http(s) paths cannot be globbed; list each file as a table",
                    table.name
                )));
            }
        }
        let url_esc = url.replace('\'', "''");
        let format = table.config.get("format").and_then(|v| v.as_str());
        let relation = format!("{}('{url_esc}')", reader_for(&url, format));
        let provider = DuckDbTableProvider::try_new(pool.clone(), relation)
            .await
            .map_err(|e| RegisterError::Other(e.to_string()))?;
        schema
            .register_table(table.name.clone(), Arc::new(provider))
            .map_err(|e| RegisterError::Other(e.to_string()))?;
        summaries.push(TableSummary {
            name: table.name.clone(),
            description: table.description.clone(),
            wiki: None,
            required_filters: Vec::new(),
        });
    }
    Ok(RegisterReport {
        table_count: summaries.len() as u64,
        tables: summaries,
        function_handle: Default::default(),
    })
}

/// Escape a DuckDB single-quoted string literal (`'` → `''`).
fn sql_quote(v: &str) -> String {
    v.replace('\'', "''")
}

/// The storage provider for secret synthesis. Explicit `storage.type` wins;
/// otherwise inferred from the first remote `path`/`location` across the source
/// config and its tables. `None` when everything is local.
fn effective_storage_type(def: &SourceDef) -> Option<String> {
    if let Some(t) = def
        .config
        .get("storage")
        .and_then(|s| s.get("type"))
        .and_then(|v| v.as_str())
    {
        return Some(t.to_string());
    }
    let from_cfg = |cfg: &JsonValue| -> Option<String> {
        cfg.get("path")
            .or_else(|| cfg.get("location"))
            .and_then(|v| v.as_str())
            .and_then(|p| StorageScheme::classify(p).default_storage_type())
            .map(str::to_string)
    };
    from_cfg(&def.config).or_else(|| def.tables.iter().find_map(|t| from_cfg(&t.config)))
}

/// Distinct `scheme://authority/` origins (for `provider`) across the source's
/// config and table paths, in first-seen order. Used as secret `SCOPE`s.
fn secret_origins(def: &SourceDef, provider: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |cfg: &JsonValue| {
        let Some(loc) = cfg
            .get("path")
            .or_else(|| cfg.get("location"))
            .and_then(|v| v.as_str())
        else {
            return;
        };
        if let Some(origin) = origin_prefix(loc) {
            if StorageScheme::classify(&origin).default_storage_type() == Some(provider)
                && !out.contains(&origin)
            {
                out.push(origin);
            }
        }
    };
    push(&def.config);
    for t in &def.tables {
        push(&t.config);
    }
    out
}

/// Sanitize a `scheme://authority/` origin into a secret-name suffix
/// (non-alphanumeric → `_`).
fn sanitize_authority(origin: &str) -> String {
    let authority = origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest.trim_end_matches('/'));
    authority
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Wrap the inner secret params (everything inside the parens, sans `SCOPE`)
/// into one `CREATE OR REPLACE SECRET` per distinct origin, each scoped to that
/// origin. Falls back to a single unscoped secret when no origin can be derived
/// (e.g. a path-less config).
fn scoped_secrets(def: &SourceDef, provider: &str, inner: &str) -> Vec<String> {
    let base = def.name.replace('"', "\"\"");
    let origins = secret_origins(def, provider);
    if origins.is_empty() {
        return vec![format!("CREATE OR REPLACE SECRET \"{base}\" ({inner})")];
    }
    origins
        .iter()
        .map(|origin| {
            let suffix = sanitize_authority(origin);
            format!(
                "CREATE OR REPLACE SECRET \"{base}__{suffix}\" ({inner}, SCOPE '{}')",
                sql_quote(origin)
            )
        })
        .collect()
}

/// Build the DuckDB `CREATE SECRET` statement(s) for a remote `file` source.
/// Dispatches on the effective storage provider; http may emit several (one per
/// host), object stores one per bucket. `Ok(vec![])` when no secret is needed.
fn build_secret_sql(def: &SourceDef) -> Result<Vec<String>, String> {
    let Some(provider) = effective_storage_type(def) else {
        return Ok(vec![]);
    };
    match provider.as_str() {
        "http" => build_http_secret_sql(def),
        "s3" | "gcs" | "azure" => Ok(build_object_store_secret_sql(def, &provider)),
        _ => Ok(vec![]),
    }
}

/// Build object-store `CREATE SECRET` statements (s3 / gcs / azure) from the
/// source's `config.storage` block — one host-scoped secret per distinct bucket
/// origin (or a single unscoped secret when none can be derived). `storage.region`
/// is a location, not a credential; credentials live under a typed `storage.auth`
/// block (`auth.type` selects the method — default `access_key`). Lenient: only
/// emits the keys present; empty when there's no storage block / nothing to emit.
fn build_object_store_secret_sql(def: &SourceDef, provider: &str) -> Vec<String> {
    let Some(storage) = def.config.get("storage") else {
        // Bare remote path with no `storage:` block → no secret; DuckDB falls
        // back to ambient credentials.
        return vec![];
    };
    let auth = storage.get("auth");
    let auth_type = auth
        .and_then(|a| a.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("access_key");
    let region = storage.get("region").and_then(|v| v.as_str());
    let from_auth = |key: &str| auth.and_then(|a| a.get(key)).and_then(|v| v.as_str());

    // (DuckDB secret key, value) pairs; `None` values are dropped.
    let pairs: Vec<(&str, Option<&str>)> = if matches!(auth_type, "credential_chain" | "chain") {
        // Let DuckDB resolve credentials from the ambient provider chain
        // (env / instance profile / gcloud / az login).
        vec![
            ("PROVIDER", Some("credential_chain")),
            ("REGION", region),
            ("ENDPOINT", from_auth("endpoint")),
            ("ACCOUNT_NAME", from_auth("account_name")),
        ]
    } else {
        // `access_key` (and any explicit-credential style): emit the keys given.
        match provider {
            "s3" => vec![
                ("KEY_ID", from_auth("access_key_id")),
                ("SECRET", from_auth("secret_access_key")),
                ("SESSION_TOKEN", from_auth("session_token")),
                ("REGION", region),
                ("ENDPOINT", from_auth("endpoint")),
                ("URL_STYLE", from_auth("url_style")),
            ],
            "gcs" => vec![
                ("KEY_ID", from_auth("access_key_id")),
                ("SECRET", from_auth("secret_access_key")),
            ],
            // azure
            _ => vec![
                ("CONNECTION_STRING", from_auth("connection_string")),
                ("ACCOUNT_NAME", from_auth("account_name")),
            ],
        }
    };

    let mut params: Vec<String> = vec![format!("TYPE {provider}")];
    for (k, v) in pairs {
        if let Some(v) = v {
            // PROVIDER is a keyword (unquoted); everything else is a quoted literal.
            if k == "PROVIDER" {
                params.push(format!("{k} {v}"));
            } else {
                params.push(format!("{k} '{}'", sql_quote(v)));
            }
        }
    }
    // Only `TYPE …` and nothing else → no credential to emit.
    if params.len() == 1 {
        return vec![];
    }
    scoped_secrets(def, provider, &params.join(", "))
}

/// Build `CREATE OR REPLACE SECRET (TYPE http, …)` statements from
/// `storage.auth`, reusing the HTTP-source `AuthSpec` taxonomy and emitting one
/// host-scoped secret per distinct http host across the source's tables.
/// `Ok(vec![])` for public files / empty auth. Rejects `custom` / `oauth2`.
fn build_http_secret_sql(def: &SourceDef) -> Result<Vec<String>, String> {
    let Some(auth_val) = def.config.get("storage").and_then(|s| s.get("auth")) else {
        return Ok(vec![]); // public file
    };
    let auth: AuthSpec = serde_json::from_value(auth_val.clone())
        .map_err(|e| format!("invalid http storage `auth`: {e}"))?;

    // BEARER_TOKEN (an `Authorization` bearer) and/or EXTRA_HTTP_HEADERS map.
    let mut bearer_token: Option<String> = None;
    let mut headers: Vec<(String, String)> = Vec::new();

    match auth {
        AuthSpec::None => return Ok(vec![]),
        AuthSpec::Header { headers: hs } => {
            for h in hs {
                if let Some(t) = h.bearer {
                    let t = t.expose_secret();
                    if h.name.eq_ignore_ascii_case("authorization") && bearer_token.is_none() {
                        bearer_token = Some(t.to_string());
                    } else {
                        headers.push((h.name, format!("Bearer {t}")));
                    }
                } else if let Some(v) = h.value {
                    headers.push((h.name, v.expose_secret().to_string()));
                }
            }
        }
        AuthSpec::Basic { username, password } => {
            let enc = base64::engine::general_purpose::STANDARD
                .encode(format!("{username}:{}", password.expose_secret()));
            headers.push(("Authorization".to_string(), format!("Basic {enc}")));
        }
        AuthSpec::Custom { .. } => {
            return Err(
                "storage auth type `custom` (query-string credentials) is not \
                 supported for http storage; DuckDB HTTP secrets carry only \
                 headers/bearer tokens"
                    .to_string(),
            );
        }
        AuthSpec::Oauth2 { .. } => {
            return Err("storage auth type `oauth2` is not yet supported for http \
                 storage; use type: header with a pre-fetched bearer token"
                .to_string());
        }
    }

    let mut params: Vec<String> = vec!["TYPE http".to_string()];
    if let Some(t) = &bearer_token {
        params.push(format!("BEARER_TOKEN '{}'", sql_quote(t)));
    }
    if !headers.is_empty() {
        let entries: Vec<String> = headers
            .iter()
            .map(|(k, v)| format!("'{}': '{}'", sql_quote(k), sql_quote(v)))
            .collect();
        params.push(format!(
            "EXTRA_HTTP_HEADERS MAP {{ {} }}",
            entries.join(", ")
        ));
    }
    // Only `TYPE http` → no credential → no secret (e.g. `headers: []`).
    if params.len() == 1 {
        return Ok(vec![]);
    }
    Ok(scoped_secrets(def, "http", &params.join(", ")))
}

/// Pick the DuckDB reader function for a URL: an explicit `format` wins;
/// otherwise infer from the extension (query string + compression suffix
/// stripped, lowercased); otherwise default to parquet.
fn reader_for(url: &str, explicit_format: Option<&str>) -> &'static str {
    if let Some(fmt) = explicit_format {
        return match fmt.to_ascii_lowercase().as_str() {
            "csv" => "read_csv",
            "json" | "ndjson" | "jsonl" => "read_json",
            _ => "read_parquet",
        };
    }
    let path = url.split('?').next().unwrap_or(url);
    let path = path
        .strip_suffix(".gz")
        .or_else(|| path.strip_suffix(".zst"))
        .or_else(|| path.strip_suffix(".bz2"))
        .unwrap_or(path);
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".csv") {
        "read_csv"
    } else if lower.ends_with(".json") || lower.ends_with(".jsonl") || lower.ends_with(".ndjson") {
        "read_json"
    } else {
        "read_parquet"
    }
}

/// Read a table location from `config.path` or `config.location`.
fn table_location(config: &JsonValue) -> Option<String> {
    config
        .get("path")
        .or_else(|| config.get("location"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Path to a local DuckDB database file for `kind: duckdb` (`config.path`).
fn duckdb_file_path(config: &JsonValue) -> Result<String, RegisterError> {
    config
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| RegisterError::Other("`kind: duckdb` requires `config.path`".to_string()))
}

/// Build a postgres / mysql connection string: either an explicit `dsn` or an
/// assembled libpq-style `key=value` string from host/port/user/password/db.
fn postgres_mysql_conn(config: &JsonValue, _scheme: &str) -> Result<String, RegisterError> {
    if let Some(dsn) = config.get("dsn").and_then(|v| v.as_str()) {
        return Ok(dsn.to_string());
    }
    let host = config.get("host").and_then(|v| v.as_str());
    let dbname = config
        .get("dbname")
        .or_else(|| config.get("database"))
        .and_then(|v| v.as_str());
    let Some(host) = host else {
        return Err(RegisterError::Other(
            "postgres/mysql require `config.dsn` or `config.host` + `config.database`".into(),
        ));
    };
    let mut parts = vec![format!("host={host}")];
    if let Some(port) = config.get("port") {
        if let Some(p) = port.as_u64() {
            parts.push(format!("port={p}"));
        } else if let Some(p) = port.as_str() {
            parts.push(format!("port={p}"));
        }
    }
    if let Some(db) = dbname {
        parts.push(format!("dbname={db}"));
    }
    if let Some(user) = config.get("user").and_then(|v| v.as_str()) {
        parts.push(format!("user={user}"));
    }
    if let Some(pw) = config.get("password").and_then(|v| v.as_str()) {
        parts.push(format!("password={pw}"));
    }
    Ok(parts.join(" "))
}

/// Build a DuckDB snowflake ATTACH connection string (`key=value;…`).
fn snowflake_conn(config: &JsonValue) -> Result<String, RegisterError> {
    let get = |k: &str| config.get(k).and_then(|v| v.as_str());
    let (Some(account), Some(user), Some(password)) =
        (get("account"), get("user"), get("password"))
    else {
        return Err(RegisterError::Other(
            "snowflake requires `config.account`, `config.user`, `config.password`".into(),
        ));
    };
    let mut parts = vec![
        format!("account={account}"),
        format!("user={user}"),
        format!("password={password}"),
    ];
    for key in ["database", "schema", "warehouse", "role"] {
        if let Some(v) = get(key) {
            parts.push(format!("{key}={v}"));
        }
    }
    Ok(parts.join(";"))
}

/// Register an arbitrary `SchemaProvider` under `name` on a memory catalog.
fn register_schema(
    catalog: &dyn CatalogProvider,
    name: &str,
    schema: Arc<dyn SchemaProvider>,
) -> Result<(), RegisterError> {
    if let Some(memory_catalog) = catalog.as_any().downcast_ref::<MemoryCatalogProvider>() {
        memory_catalog
            .register_schema(name, schema)
            .map_err(|e| RegisterError::Other(e.to_string()))?;
        Ok(())
    } else {
        Err(RegisterError::Other(
            "catalog does not support schema registration".into(),
        ))
    }
}

/// Ensure a `MemorySchemaProvider` exists under `name` and return it (mirrors
/// the sqlite source's `ensure_schema`).
fn ensure_memory_schema(
    catalog: &dyn CatalogProvider,
    name: &str,
) -> Result<Arc<dyn SchemaProvider>, RegisterError> {
    if let Some(s) = catalog.schema(name) {
        return Ok(s);
    }
    let s: Arc<dyn SchemaProvider> = Arc::new(MemorySchemaProvider::new());
    register_schema(catalog, name, s.clone())?;
    Ok(s)
}

/// Extract a simple `column = literal` predicate, returning the column name and
/// a SQL-safe literal (strings single-quoted with `'` doubled; numbers/bools
/// inlined bare). Mirrors the sqlite source's `extract_eq_literal` but emits a
/// ready-to-inline literal rather than a bind parameter.
fn extract_eq_literal(expr: &Expr) -> Option<(String, String)> {
    use datafusion::logical_expr::{BinaryExpr, Operator};
    use datafusion::scalar::ScalarValue;
    if let Expr::BinaryExpr(BinaryExpr { left, op, right }) = expr
        && matches!(op, Operator::Eq)
    {
        let (col, scalar) = match (left.as_ref(), right.as_ref()) {
            (Expr::Column(c), Expr::Literal(s, _)) => (c, s),
            (Expr::Literal(s, _), Expr::Column(c)) => (c, s),
            _ => return None,
        };
        let value = match scalar {
            ScalarValue::Utf8(Some(s)) | ScalarValue::LargeUtf8(Some(s)) => {
                format!("'{}'", s.replace('\'', "''"))
            }
            ScalarValue::Int32(Some(n)) => n.to_string(),
            ScalarValue::Int64(Some(n)) => n.to_string(),
            ScalarValue::Float64(Some(n)) => n.to_string(),
            ScalarValue::Boolean(Some(b)) => b.to_string(),
            _ => return None,
        };
        return Some((col.name.clone(), value));
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "tests"
    )]

    use super::*;
    use datafusion::execution::context::SessionContext;
    use datafusion::logical_expr::{BinaryExpr, Operator, col, lit};
    use futures::TryStreamExt;

    #[test]
    fn postgres_dsn_passthrough() {
        let cfg = serde_json::json!({ "dsn": "postgresql://u:p@h/db" });
        let conn = postgres_mysql_conn(&cfg, "postgresql").unwrap();
        assert_eq!(conn, "postgresql://u:p@h/db");
    }

    fn file_def(config: serde_json::Value) -> SourceDef {
        SourceDef {
            name: "lake".into(),
            kind: SourceKind::File,
            description: None,
            wiki: None,
            examples: Vec::new(),
            config,
            cache: pawrly_core::CachePolicy::None,
            safety: None,
            tables: Vec::new(),
            raw_table: false,
            raw_table_safety: None,
        }
    }

    fn file_def_with_tables(config: serde_json::Value, paths: &[&str]) -> SourceDef {
        let tables = paths
            .iter()
            .enumerate()
            .map(|(i, p)| pawrly_core::TableDef {
                name: format!("t{i}"),
                description: None,
                wiki: None,
                config: serde_json::json!({ "path": p }),
                cache: None,
                safety: None,
            })
            .collect();
        SourceDef {
            tables,
            ..file_def(config)
        }
    }

    /// Join the emitted secret statements for assertion convenience.
    fn secret_sql(def: &SourceDef) -> String {
        build_secret_sql(def).unwrap().join("\n")
    }

    #[test]
    fn storage_access_key_auth() {
        let def = file_def(serde_json::json!({
            "storage": {
                "type": "s3",
                "region": "us-east-1",
                "auth": {
                    "type": "access_key",
                    "access_key_id": "AKIA",
                    "secret_access_key": "shh",
                    "endpoint": "https://minio.local"
                }
            }
        }));
        let sql = secret_sql(&def);
        assert!(sql.contains("TYPE s3"), "{sql}");
        assert!(sql.contains("KEY_ID 'AKIA'"), "{sql}");
        assert!(sql.contains("SECRET 'shh'"), "{sql}");
        assert!(sql.contains("REGION 'us-east-1'"), "{sql}");
        assert!(sql.contains("ENDPOINT 'https://minio.local'"), "{sql}");
    }

    #[test]
    fn storage_credential_chain_auth() {
        let def = file_def(serde_json::json!({
            "storage": { "type": "s3", "region": "eu-west-1", "auth": { "type": "credential_chain" } }
        }));
        let sql = secret_sql(&def);
        assert!(sql.contains("PROVIDER credential_chain"), "{sql}");
        // PROVIDER is a keyword, not a quoted literal.
        assert!(
            !sql.contains("'credential_chain'"),
            "provider must be unquoted: {sql}"
        );
        assert!(sql.contains("REGION 'eu-west-1'"), "{sql}");
    }

    #[test]
    fn storage_default_auth_is_access_key() {
        // No `auth.type` → defaults to access_key.
        let def = file_def(serde_json::json!({
            "storage": { "type": "gcs", "auth": { "access_key_id": "k", "secret_access_key": "s" } }
        }));
        let sql = secret_sql(&def);
        assert!(sql.contains("TYPE gcs"), "{sql}");
        assert!(
            sql.contains("KEY_ID 'k'") && sql.contains("SECRET 's'"),
            "{sql}"
        );
    }

    #[test]
    fn no_storage_block_no_secret() {
        let def = file_def(serde_json::json!({ "path": "./x/*.parquet" }));
        assert!(build_secret_sql(&def).unwrap().is_empty());
    }

    #[test]
    fn pathless_config_falls_back_to_unscoped() {
        // No table paths → single unscoped secret named after the source.
        let def = file_def(serde_json::json!({
            "storage": { "type": "s3", "auth": { "access_key_id": "k", "secret_access_key": "s" } }
        }));
        let sql = secret_sql(&def);
        assert!(sql.contains("SECRET \"lake\" (TYPE s3"), "{sql}");
        assert!(!sql.contains("SCOPE"), "no scope without a path: {sql}");
    }

    #[test]
    fn s3_secret_scoped_to_bucket() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "s3", "auth": { "access_key_id": "k", "secret_access_key": "s" } }
            }),
            &["s3://bucket/data.parquet"],
        );
        let sql = secret_sql(&def);
        assert!(sql.contains("SECRET \"lake__bucket\""), "{sql}");
        assert!(sql.contains("SCOPE 's3://bucket/'"), "{sql}");
    }

    #[test]
    fn gcs_scheme_preserved_in_scope() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "gcs", "auth": { "access_key_id": "k", "secret_access_key": "s" } }
            }),
            &["gs://bkt/data.parquet"],
        );
        assert!(
            secret_sql(&def).contains("SCOPE 'gs://bkt/'"),
            "{}",
            secret_sql(&def)
        );
    }

    #[test]
    fn azure_abfss_scope_keeps_container() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "azure", "auth": { "connection_string": "cs" } }
            }),
            &["abfss://container@acct.dfs.core.windows.net/data.parquet"],
        );
        let sql = secret_sql(&def);
        assert!(
            sql.contains("SCOPE 'abfss://container@acct.dfs.core.windows.net/'"),
            "{sql}"
        );
    }

    #[test]
    fn multi_bucket_emits_one_secret_each() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "s3", "auth": { "access_key_id": "k", "secret_access_key": "s" } }
            }),
            &[
                "s3://b1/a.parquet",
                "s3://b2/b.parquet",
                "s3://b1/c.parquet",
            ],
        );
        let stmts = build_secret_sql(&def).unwrap();
        assert_eq!(stmts.len(), 2, "one per distinct bucket: {stmts:?}");
        assert!(stmts.iter().any(|s| s.contains("SCOPE 's3://b1/'")));
        assert!(stmts.iter().any(|s| s.contains("SCOPE 's3://b2/'")));
    }

    #[test]
    fn http_no_auth_no_secret() {
        let def = file_def_with_tables(
            serde_json::json!({ "storage": { "type": "http" } }),
            &["https://h/a.parquet"],
        );
        assert!(build_secret_sql(&def).unwrap().is_empty());
    }

    #[test]
    fn http_public_path_no_storage_block() {
        // No storage block, inferred http from path → public, no secret.
        let def = file_def_with_tables(serde_json::json!({}), &["https://h/a.parquet"]);
        assert_eq!(effective_storage_type(&def).as_deref(), Some("http"));
        assert!(build_secret_sql(&def).unwrap().is_empty());
    }

    #[test]
    fn http_header_bearer_uses_bearer_token() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "http", "auth": {
                    "type": "header",
                    "headers": [{ "name": "Authorization", "bearer": "tok" }]
                } }
            }),
            &["https://api.example.com/d.parquet"],
        );
        let sql = secret_sql(&def);
        assert!(sql.contains("TYPE http"), "{sql}");
        assert!(sql.contains("BEARER_TOKEN 'tok'"), "{sql}");
        assert!(
            !sql.contains("EXTRA_HTTP_HEADERS"),
            "lone bearer → no map: {sql}"
        );
        assert!(sql.contains("SCOPE 'https://api.example.com/'"), "{sql}");
        assert!(sql.contains("SECRET \"lake__api_example_com\""), "{sql}");
    }

    #[test]
    fn http_header_verbatim_map() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "http", "auth": {
                    "type": "header",
                    "headers": [{ "name": "X-Api-Key", "value": "abc" }]
                } }
            }),
            &["https://h/d.csv"],
        );
        let sql = secret_sql(&def);
        assert!(
            sql.contains("EXTRA_HTTP_HEADERS MAP { 'X-Api-Key': 'abc' }"),
            "{sql}"
        );
        assert!(!sql.contains("BEARER_TOKEN"), "{sql}");
    }

    #[test]
    fn http_basic_base64() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "http", "auth": {
                    "type": "basic", "username": "u", "password": "p"
                } }
            }),
            &["https://h/d.parquet"],
        );
        // base64("u:p") == "dTpw"
        assert!(
            secret_sql(&def).contains("MAP { 'Authorization': 'Basic dTpw' }"),
            "{}",
            secret_sql(&def)
        );
    }

    #[test]
    fn http_custom_rejected() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "http", "auth": {
                    "type": "custom", "query": [{ "name": "api_key", "value": "x" }]
                } }
            }),
            &["https://h/d.parquet"],
        );
        let err = build_secret_sql(&def).unwrap_err();
        assert!(err.contains("custom"), "{err}");
    }

    #[test]
    fn http_oauth2_rejected() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "http", "auth": {
                    "type": "oauth2", "token_url": "https://t", "client_id": "c", "client_secret": "s"
                } }
            }),
            &["https://h/d.parquet"],
        );
        let err = build_secret_sql(&def).unwrap_err();
        assert!(err.contains("oauth2"), "{err}");
    }

    #[test]
    fn http_header_quote_escaping() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "http", "auth": {
                    "type": "header",
                    "headers": [{ "name": "X-K", "value": "a'b" }]
                } }
            }),
            &["https://h/d.parquet"],
        );
        assert!(
            secret_sql(&def).contains("'X-K': 'a''b'"),
            "{}",
            secret_sql(&def)
        );
    }

    #[test]
    fn http_empty_headers_no_secret() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "http", "auth": { "type": "header", "headers": [] } }
            }),
            &["https://h/d.parquet"],
        );
        assert!(build_secret_sql(&def).unwrap().is_empty());
    }

    #[test]
    fn http_multi_host_emits_one_secret_each() {
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "type": "http", "auth": {
                    "type": "header",
                    "headers": [{ "name": "Authorization", "bearer": "t" }]
                } }
            }),
            &["https://a.com/x.csv", "https://b.com/y.csv"],
        );
        let stmts = build_secret_sql(&def).unwrap();
        assert_eq!(stmts.len(), 2, "{stmts:?}");
        assert!(stmts.iter().any(|s| s.contains("SCOPE 'https://a.com/'")));
        assert!(stmts.iter().any(|s| s.contains("SCOPE 'https://b.com/'")));
    }

    #[test]
    fn auth_without_type_infers_http_from_path() {
        // `auth` present, `type` omitted → inferred from the https path.
        let def = file_def_with_tables(
            serde_json::json!({
                "storage": { "auth": {
                    "type": "header",
                    "headers": [{ "name": "Authorization", "bearer": "t" }]
                } }
            }),
            &["https://api.example.com/d.csv"],
        );
        assert_eq!(effective_storage_type(&def).as_deref(), Some("http"));
        assert!(
            secret_sql(&def).contains("TYPE http"),
            "{}",
            secret_sql(&def)
        );
    }

    #[test]
    fn effective_storage_type_explicit_wins() {
        let def = file_def_with_tables(
            serde_json::json!({ "storage": { "type": "s3" } }),
            &["https://h/x.parquet"],
        );
        assert_eq!(effective_storage_type(&def).as_deref(), Some("s3"));
    }

    #[test]
    fn effective_storage_type_inference() {
        let s3 = file_def_with_tables(serde_json::json!({}), &["s3://b/k.parquet"]);
        assert_eq!(effective_storage_type(&s3).as_deref(), Some("s3"));
        let local = file_def(serde_json::json!({ "path": "./x.parquet" }));
        assert_eq!(effective_storage_type(&local), None);
    }

    #[test]
    fn reader_for_inference() {
        assert_eq!(reader_for("https://h/a.CSV", None), "read_csv");
        assert_eq!(reader_for("https://h/a.csv.gz", None), "read_csv");
        assert_eq!(reader_for("https://h/a.jsonl.zst", None), "read_json");
        assert_eq!(
            reader_for("https://h/a.parquet?sig=x", None),
            "read_parquet"
        );
        assert_eq!(reader_for("https://h/a.json", None), "read_json");
        // Extensionless → parquet default.
        assert_eq!(
            reader_for("https://h/export?type=parquet", None),
            "read_parquet"
        );
        // Explicit format wins over extension.
        assert_eq!(reader_for("https://h/a.parquet", Some("csv")), "read_csv");
        assert_eq!(reader_for("https://h/a.csv", Some("json")), "read_json");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn attach_source_enumerates_tables() {
        // A `kind: duckdb` ATTACH should populate RegisterReport.tables (so the
        // catalog shows in `schema`/MCP), not report an empty list.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cat.duckdb");
        {
            let conn = duckdb::Connection::open(&db_path).unwrap();
            conn.execute_batch("CREATE TABLE alpha(id INTEGER); CREATE TABLE beta(name VARCHAR);")
                .unwrap();
        }
        let def = SourceDef {
            name: "lake".into(),
            kind: SourceKind::Duckdb,
            description: None,
            wiki: None,
            examples: Vec::new(),
            config: serde_json::json!({ "path": db_path.to_string_lossy() }),
            cache: pawrly_core::CachePolicy::None,
            safety: None,
            tables: Vec::new(),
            raw_table: false,
            raw_table_safety: None,
        };
        let pool = Arc::new(DuckDbPool::with_offline(2, true).unwrap());
        let catalog = MemoryCatalogProvider::new();
        let report = register_duckdb_source(&def, &pool, &catalog, std::path::Path::new("/"))
            .await
            .unwrap();
        let mut names: Vec<String> = report.tables.into_iter().map(|t| t.name).collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);
        assert_eq!(report.table_count, 2);
    }

    #[test]
    fn resolve_local_rules() {
        let ws = std::path::Path::new("/work/space");
        // Relative local path → joined to the workspace dir.
        assert_eq!(resolve_local(ws, "./a.duckdb"), "/work/space/./a.duckdb");
        // Absolute path → unchanged.
        assert_eq!(resolve_local(ws, "/data/a.duckdb"), "/data/a.duckdb");
        // Remote URL → unchanged.
        assert_eq!(
            resolve_local(ws, "s3://bucket/x.parquet"),
            "s3://bucket/x.parquet"
        );
    }

    #[test]
    fn postgres_assembled_conn() {
        let cfg = serde_json::json!({
            "host": "db.example.com",
            "port": 5433,
            "database": "analytics",
            "user": "ro",
            "password": "secret"
        });
        let conn = postgres_mysql_conn(&cfg, "postgresql").unwrap();
        assert_eq!(
            conn,
            "host=db.example.com port=5433 dbname=analytics user=ro password=secret"
        );
    }

    #[test]
    fn postgres_missing_host_errors() {
        let cfg = serde_json::json!({ "user": "ro" });
        assert!(postgres_mysql_conn(&cfg, "postgresql").is_err());
    }

    #[test]
    fn snowflake_key_value_assembly() {
        let cfg = serde_json::json!({
            "account": "ACME",
            "user": "svc",
            "password": "pw",
            "database": "DB",
            "schema": "PUBLIC",
            "warehouse": "WH",
            "role": "R"
        });
        let conn = snowflake_conn(&cfg).unwrap();
        assert_eq!(
            conn,
            "account=ACME;user=svc;password=pw;database=DB;schema=PUBLIC;warehouse=WH;role=R"
        );
    }

    #[test]
    fn snowflake_missing_required_errors() {
        let cfg = serde_json::json!({ "account": "ACME" });
        assert!(snowflake_conn(&cfg).is_err());
    }

    #[test]
    fn eq_literal_escapes_strings() {
        let expr = Expr::BinaryExpr(BinaryExpr {
            left: Box::new(col("name")),
            op: Operator::Eq,
            right: Box::new(lit("O'Brien")),
        });
        let (c, v) = extract_eq_literal(&expr).unwrap();
        assert_eq!(c, "name");
        assert_eq!(v, "'O''Brien'");
    }

    #[test]
    fn eq_literal_inlines_numbers() {
        let expr = Expr::BinaryExpr(BinaryExpr {
            left: Box::new(col("id")),
            op: Operator::Eq,
            right: Box::new(lit(42_i64)),
        });
        let (c, v) = extract_eq_literal(&expr).unwrap();
        assert_eq!(c, "id");
        assert_eq!(v, "42");
    }

    #[test]
    fn eq_literal_rejects_non_eq() {
        let expr = Expr::BinaryExpr(BinaryExpr {
            left: Box::new(col("id")),
            op: Operator::Gt,
            right: Box::new(lit(1_i64)),
        });
        assert!(extract_eq_literal(&expr).is_none());
    }

    #[tokio::test]
    async fn build_sql_projects_filters_and_limits() {
        let pool = Arc::new(DuckDbPool::with_offline(2, true).unwrap());
        pool.execute("CREATE TABLE t AS SELECT 1 AS id, 'a' AS name UNION ALL SELECT 2, 'b'")
            .await
            .unwrap();
        let provider = DuckDbTableProvider::try_new(pool, "t".to_string())
            .await
            .unwrap();
        // Projection of column 1 ("name") with an eq filter on id.
        let filter = Expr::BinaryExpr(BinaryExpr {
            left: Box::new(col("id")),
            op: Operator::Eq,
            right: Box::new(lit(2_i64)),
        });
        let sql = provider.build_sql(Some(&vec![1]), &[filter], Some(10));
        assert_eq!(sql, "SELECT \"name\" FROM t WHERE \"id\" = 2 LIMIT 10");
        // No projection -> `*`.
        let sql = provider.build_sql(None, &[], None);
        assert_eq!(sql, "SELECT * FROM t");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn end_to_end_scan_pushes_predicate() {
        let pool = Arc::new(DuckDbPool::with_offline(2, true).unwrap());
        pool.execute("CREATE TABLE t AS SELECT 1 AS id, 'a' AS name UNION ALL SELECT 2, 'b'")
            .await
            .unwrap();
        let provider = Arc::new(
            DuckDbTableProvider::try_new(pool, "t".to_string())
                .await
                .unwrap(),
        );

        // Schema inference works.
        let schema = provider.schema();
        assert_eq!(schema.fields().len(), 2);
        assert_eq!(schema.field(0).name(), "id");
        assert_eq!(schema.field(1).name(), "name");

        let ctx = SessionContext::new();
        let state = ctx.state();

        // WHERE id = 2 pushes the predicate and returns exactly one row.
        let filter = Expr::BinaryExpr(BinaryExpr {
            left: Box::new(col("id")),
            op: Operator::Eq,
            right: Box::new(lit(2_i64)),
        });
        let exec = provider.scan(&state, None, &[filter], None).await.unwrap();
        let batches: Vec<_> = exec
            .execute(0, ctx.task_ctx())
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        let rows: usize = batches.iter().map(arrow_array::RecordBatch::num_rows).sum();
        assert_eq!(rows, 1, "predicate pushdown should leave one row");

        // Projection returns just the requested column.
        let exec = provider
            .scan(&state, Some(&vec![1]), &[], None)
            .await
            .unwrap();
        assert_eq!(exec.schema().fields().len(), 1);
        assert_eq!(exec.schema().field(0).name(), "name");
        let batches: Vec<_> = exec
            .execute(0, ctx.task_ctx())
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        assert_eq!(batches[0].num_columns(), 1);
    }
}
