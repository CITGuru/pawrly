//! DuckDB-backed sources: postgres / mysql / snowflake / duckdb (ATTACH
//! databases), iceberg / delta (scan functions), ducklake (DuckLake catalog),
//! and object storage for remote `file` sources (`read_parquet`/csv/json).
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
use pawrly_core::{SourceDef, SourceKind};
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
            Some(p) if !p.is_empty() => p
                .iter()
                .map(|i| format!("\"{}\"", self.schema.field(*i).name()))
                .collect::<Vec<_>>()
                .join(", "),
            _ => "*".to_string(),
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
        use futures::stream::TryStreamExt;
        let pool = self.pool.clone();
        let sql = self.sql.clone();
        let schema = self.schema.clone();
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
        .try_flatten();
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
) -> Result<RegisterReport, RegisterError> {
    match def.kind {
        SourceKind::Postgres | SourceKind::Mysql | SourceKind::Snowflake | SourceKind::Duckdb => {
            register_attach(def, pool, catalog).await
        }
        SourceKind::Iceberg | SourceKind::Delta => register_scan(def, pool, catalog).await,
        SourceKind::Ducklake => register_ducklake(def, pool, catalog).await,
        SourceKind::File => register_object_store(def, pool, catalog).await,
        other => Err(RegisterError::Other(format!(
            "register_duckdb_source called for unsupported kind `{other}`"
        ))),
    }
}

/// ATTACH a foreign database (postgres / mysql / snowflake) or a local DuckDB
/// database file (`duckdb`), and expose its tables lazily via a
/// [`DuckDbSchemaProvider`].
async fn register_attach(
    def: &SourceDef,
    pool: &Arc<DuckDbPool>,
    catalog: &dyn CatalogProvider,
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
        SourceKind::Duckdb => (None, None, duckdb_file_path(&def.config)?),
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

    // Enumeration is lazy; report an empty table list.
    Ok(RegisterReport {
        table_count: 0,
        tables: Vec::new(),
    })
}

/// Iceberg / delta: each declared table maps to a scan function over a path.
async fn register_scan(
    def: &SourceDef,
    pool: &Arc<DuckDbPool>,
    catalog: &dyn CatalogProvider,
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
        let loc_esc = loc.replace('\'', "''");
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
            required_filters: Vec::new(),
        });
    }
    Ok(RegisterReport {
        table_count: summaries.len() as u64,
        tables: summaries,
    })
}

/// DuckLake catalog: ATTACH `'ducklake:<catalog>'` (optionally with a
/// `DATA_PATH`), then expose its tables lazily via a [`DuckDbSchemaProvider`].
async fn register_ducklake(
    def: &SourceDef,
    pool: &Arc<DuckDbPool>,
    catalog: &dyn CatalogProvider,
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
        if let Some(secret) = build_secret_sql(def) {
            pool.execute(&secret)
                .await
                .map_err(|e| RegisterError::Other(e.to_string()))?;
        }
    }

    let catalog_esc = catalog_uri.replace('\'', "''");
    let name = &def.name;
    let attach = match def.config.get("data_path").and_then(|v| v.as_str()) {
        Some(data_path) => {
            let dp_esc = data_path.replace('\'', "''");
            format!("ATTACH 'ducklake:{catalog_esc}' AS \"{name}\" (DATA_PATH '{dp_esc}')")
        }
        None => format!("ATTACH 'ducklake:{catalog_esc}' AS \"{name}\""),
    };
    pool.execute(&attach)
        .await
        .map_err(|e| RegisterError::Other(e.to_string()))?;

    let schema: Arc<dyn SchemaProvider> =
        Arc::new(DuckDbSchemaProvider::new(pool.clone(), def.name.clone()));
    register_schema(catalog, &def.name, schema)?;
    Ok(RegisterReport {
        table_count: 0,
        tables: Vec::new(),
    })
}

/// Remote `file` over an object store: create a secret from the `storage:`
/// block, then `read_parquet` (or csv/json) each declared table's URL.
async fn register_object_store(
    def: &SourceDef,
    pool: &Arc<DuckDbPool>,
    catalog: &dyn CatalogProvider,
) -> Result<RegisterReport, RegisterError> {
    pool.ensure_extension("httpfs")
        .await
        .map_err(|e| RegisterError::Other(e.to_string()))?;

    if let Some(secret) = build_secret_sql(def) {
        pool.execute(&secret)
            .await
            .map_err(|e| RegisterError::Other(e.to_string()))?;
    }

    let schema = ensure_memory_schema(catalog, &def.name)?;
    let mut summaries = Vec::with_capacity(def.tables.len());
    for table in &def.tables {
        let url = table_location(&table.config).ok_or_else(|| {
            RegisterError::Other(format!(
                "table `{}` is missing `path`/`location`",
                table.name
            ))
        })?;
        let url_esc = url.replace('\'', "''");
        let format = table
            .config
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("parquet");
        let relation = match format {
            "csv" => format!("read_csv('{url_esc}')"),
            "json" | "ndjson" => format!("read_json('{url_esc}')"),
            _ => format!("read_parquet('{url_esc}')"),
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
            required_filters: Vec::new(),
        });
    }
    Ok(RegisterReport {
        table_count: summaries.len() as u64,
        tables: summaries,
    })
}

/// Build a DuckDB `CREATE SECRET` for an object store from the source's
/// `config.storage` block, or `None` when there's no storage block / nothing to
/// emit. The provider is `storage.type` (`s3` | `gcs` | `azure`); `storage.region`
/// sits at the storage level, and credentials live under a typed `storage.auth`
/// block (`auth.type` selects the method — default `access_key`). Lenient: only
/// emits the keys that are present.
fn build_secret_sql(def: &SourceDef) -> Option<String> {
    let storage = def.config.get("storage")?;
    let provider = storage.get("type").and_then(|v| v.as_str())?;
    if !matches!(provider, "s3" | "gcs" | "azure") {
        return None;
    }
    let auth = storage.get("auth");
    let auth_type = auth
        .and_then(|a| a.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("access_key");
    // `region` is a location, not a credential, so it lives at the storage level.
    let region = storage.get("region").and_then(|v| v.as_str());
    let from_auth = |key: &str| auth.and_then(|a| a.get(key)).and_then(|v| v.as_str());
    let q = |v: &str| v.replace('\'', "''");

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

    let params: Vec<String> = pairs
        .into_iter()
        .filter_map(|(k, v)| {
            v.map(|v| {
                // PROVIDER is a keyword (unquoted); everything else is a quoted literal.
                if k == "PROVIDER" {
                    format!("{k} {v}")
                } else {
                    format!("{k} '{}'", q(v))
                }
            })
        })
        .collect();
    if params.is_empty() {
        return None;
    }
    let name = def.name.replace('"', "\"\"");
    Some(format!(
        "CREATE OR REPLACE SECRET \"{name}\" (TYPE {provider}, {})",
        params.join(", ")
    ))
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
            config,
            cache: pawrly_core::CachePolicy::None,
            safety: None,
            tables: Vec::new(),
            raw_table: false,
            raw_table_safety: None,
        }
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
        let sql = build_secret_sql(&def).unwrap();
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
        let sql = build_secret_sql(&def).unwrap();
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
        let sql = build_secret_sql(&def).unwrap();
        assert!(sql.contains("TYPE gcs"), "{sql}");
        assert!(
            sql.contains("KEY_ID 'k'") && sql.contains("SECRET 's'"),
            "{sql}"
        );
    }

    #[test]
    fn no_storage_block_no_secret() {
        let def = file_def(serde_json::json!({ "path": "./x/*.parquet" }));
        assert!(build_secret_sql(&def).is_none());
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
