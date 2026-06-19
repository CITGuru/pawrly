//! `LocalEngine` — in-process implementation of `EngineService`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use arrow_schema::SchemaRef;
use async_trait::async_trait;
use chrono::Utc;
use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider, SchemaProvider};
use datafusion::execution::context::SessionContext;
use parking_lot::{Mutex, RwLock};
use pawrly_core::semantic::{SemanticModelDescription, SemanticModelInfo, SemanticQuery};
use pawrly_core::{
    CacheEntryInfo, CachePolicy, CatalogSnapshot, ColumnSpec, EngineError, EngineService,
    HealthReport, MaterializeOutcome, MaterializeSpec, QueryId, QueryRequest, QueryStream,
    RefreshCatalogOutcome, RefreshOutcome, ReloadReport, SchemaSummary, SourceDef, SourceInfo,
    SourceStatus, SourceTestReport, TableDescription, TableFilter, TableInfo, TableName,
    TableSummary, VacuumReport,
};
use pawrly_semantic::SemanticCatalog;
use tokio::task::JoinHandle;

use crate::cache::CacheManager;
use crate::duckdb_pool::DuckDbPool;
use crate::registry;

const PAWRLY_CATALOG: &str = "pawrly";

/// Configuration for [`LocalEngine::new`].
#[derive(Debug, Clone)]
pub struct LocalEngineConfig {
    /// The parsed (and secret-resolved) workspace config.
    pub config: pawrly_config::Config,
    /// Workspace directory (used to resolve relative source paths).
    pub workspace_dir: PathBuf,
    /// DuckDB connection pool size. `None` defaults to `num_cpus::get()`.
    pub duckdb_pool_size: Option<usize>,
    /// Pawrly home directory (`--home` / `$PAWRLY_HOME`). `None` resolves via
    /// [`pawrly_core::resolve_home`] (env var, then `~/.pawrly`). Drives the
    /// default cache storage root (`<home>/cache`) and marks the home-based
    /// config as the `default` workspace.
    pub home: Option<PathBuf>,
}

impl LocalEngineConfig {
    /// Resolved DuckDB pool size, defaulting to `num_cpus::get()`.
    fn resolved_pool_size(&self) -> usize {
        self.duckdb_pool_size
            .filter(|n| *n > 0)
            .unwrap_or_else(num_cpus::get)
            .max(1)
    }
}

/// In-process engine wrapping DataFusion.
pub struct LocalEngine {
    inner: Arc<LocalEngineInner>,
}

impl std::fmt::Debug for LocalEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalEngine")
            .field("sources", &self.inner.sources.read().len())
            .finish()
    }
}

pub(crate) struct LocalEngineInner {
    pub(crate) ctx: SessionContext,
    pub(crate) catalog: Arc<MemoryCatalogProvider>,
    sources: RwLock<HashMap<String, RegisteredSource>>,
    workspace_dir: PathBuf,
    pub(crate) cache: Arc<CacheManager>,
    /// Compiled semantic-layer models. Empty when no `semantic:` block exists.
    pub(crate) semantic: Arc<SemanticCatalog>,
    /// Background cache refreshers keyed by source name (one entry per
    /// `refresh`/`cron` table) plus a `__rollups__` bucket for pre-agg
    /// refreshers. Aborted on shutdown, source removal, and before a source is
    /// re-registered so re-registration never leaks tasks.
    pub(crate) refreshers: Mutex<HashMap<String, Vec<JoinHandle<()>>>>,
    /// Path the config was loaded from, when known. `reload_config` re-reads it.
    config_path: Option<PathBuf>,
    duckdb: Arc<DuckDbPool>,
    /// Recognize the inline `-- pawrly: materialize <name>` directive.
    allow_inline_materialize: bool,
    /// In-flight query count, incremented at query start and decremented when
    /// the result stream finishes or is dropped. Read by `health()`.
    active_queries: Arc<AtomicI64>,
    /// Activity-log sink (disabled unless `observability.activity` is enabled).
    activity: crate::activity::ActivitySink,
    /// SQL redaction policy for activity records.
    redact_sql: crate::redact::RedactMode,
    /// Durable activity store, when configured. Held so its buffered tail is
    /// flushed to disk when the engine is torn down.
    activity_durable: Option<crate::durable_activity::DurableActivityStore>,
}

impl Drop for LocalEngineInner {
    fn drop(&mut self) {
        // Persist the not-yet-flushed buffer on shutdown so the records recorded
        // since the last threshold/timer flush aren't lost.
        if let Some(store) = &self.activity_durable
            && let Err(e) = store.flush()
        {
            tracing::warn!(error = %e, "activity: flush on shutdown failed");
        }
    }
}

#[derive(Clone)]
struct RegisteredSource {
    info: SourceInfo,
    tables: Vec<registry::TableSummary>,
    /// Original `SourceDef`, kept so the source can be re-registered on
    /// `refresh_catalog` / `reload_config`.
    def: SourceDef,
}

impl LocalEngine {
    /// Build the activity context for an operation, or `None` when activity
    /// logging is off. `sql` is the user-submitted text (pre-substitution); it
    /// is redacted here per policy. `None` for operations without SQL text.
    /// Redact a SQL string for the activity log per the configured policy,
    /// bumping the failure metric if redaction degraded.
    fn redact_activity_sql(&self, sql: &str) -> Option<String> {
        let redacted = crate::redact::redact(sql, self.inner.redact_sql);
        if redacted.degraded {
            pawrly_telemetry::metrics::redaction_failed().add(1, &[]);
        }
        redacted.sql
    }

    fn activity_context(
        &self,
        ctx: &pawrly_core::activity::RequestContext,
        operation: pawrly_core::activity::Operation,
        sql: Option<&str>,
        params: &HashMap<String, String>,
    ) -> Option<crate::stream::ActivityContext> {
        if !self.inner.activity.is_enabled() {
            return None;
        }
        let sql = sql.and_then(|s| self.redact_activity_sql(s));
        let mut param_keys: Vec<String> = params.keys().cloned().collect();
        param_keys.sort();
        Some(crate::stream::ActivityContext {
            sink: self.inner.activity.clone(),
            id: uuid::Uuid::new_v4().to_string(),
            interface: ctx.interface,
            principal: ctx.principal.clone(),
            operation,
            sql,
            param_keys,
            trace_id: ctx
                .traceparent
                .as_deref()
                .and_then(trace_id_from_traceparent),
        })
    }

    /// Build a new local engine and register every source from the config.
    pub async fn new(cfg: LocalEngineConfig) -> Result<Self, EngineError> {
        Self::build(cfg, None).await
    }

    async fn build(
        cfg: LocalEngineConfig,
        config_path: Option<PathBuf>,
    ) -> Result<Self, EngineError> {
        use datafusion::execution::config::SessionConfig;

        let session_config = SessionConfig::new()
            .with_default_catalog_and_schema(PAWRLY_CATALOG, "default")
            .with_create_default_catalog_and_schema(false)
            .with_information_schema(true);
        let mut ctx = SessionContext::new_with_config(session_config);
        // Register the JSON SQL functions so `json`-typed columns (stored as
        // Utf8) are queryable in SQL.
        crate::json_udf::register(&mut ctx)
            .map_err(|e| EngineError::Internal(format!("register json udfs: {e}")))?;
        let catalog: Arc<MemoryCatalogProvider> = Arc::new(MemoryCatalogProvider::new());
        // Register a `default` schema so `SELECT * FROM unqualified_table` resolves.
        let default_schema: Arc<dyn datafusion::catalog::SchemaProvider> =
            Arc::new(datafusion::catalog::MemorySchemaProvider::new());
        let _ = catalog
            .register_schema("default", default_schema)
            .map_err(|e| EngineError::Internal(format!("register default schema: {e}")))?;
        ctx.register_catalog(PAWRLY_CATALOG, catalog.clone());

        // The cache root comes from `defaults.cache.storage` when set (with
        // `~` / `~/` expanded against `$HOME`), otherwise `<home>/cache` under
        // the resolved Pawrly home — NOT the workspace dir, so cached data
        // lives under the home regardless of where the CLI is invoked from. A
        // per-workspace namespace segment is then appended so different
        // workspaces sharing the same storage root never collide on identical
        // `schema.table` keys.
        let home = pawrly_core::resolve_home(cfg.home.as_deref());
        let storage = match (&cfg.config.defaults.cache.storage, &home) {
            (Some(explicit), _) => expand_tilde(explicit),
            (None, Some(h)) => h.join("cache"),
            (None, None) => {
                return Err(EngineError::Internal(
                    "cannot resolve the cache storage root: set `defaults.cache.storage` \
                     in pawrly.yaml, or set $PAWRLY_HOME or $HOME"
                        .into(),
                ));
            }
        };
        let namespace = cache_namespace(
            cfg.config.defaults.cache.namespace.as_deref(),
            &cfg.workspace_dir,
            home.as_deref(),
        );
        let cache_root = storage.join(&namespace);
        let cache = Arc::new(
            CacheManager::new(cache_root)
                .map_err(|e| EngineError::Internal(format!("cache init: {e}")))?,
        );

        // The read-only namespace catalog gives cached snapshots a second,
        // SQL-addressable face at `<namespace>.<source>.<table>`, without
        // touching the transparent read-through path. Registered under the same
        // per-workspace namespace string that segments the on-disk cache.
        ctx.register_catalog(
            &namespace,
            Arc::new(crate::namespace::NamespaceCatalogProvider::new(
                cache.clone(),
            )),
        );
        // Also expose `materialized.<name>` in the default catalog so materialized
        // tables resolve without the namespace prefix.
        let _ = catalog.register_schema(
            pawrly_core::MATERIALIZED_SCHEMA,
            crate::namespace::schema_provider_for(cache.clone(), pawrly_core::MATERIALIZED_SCHEMA),
        );

        let duckdb = Arc::new(DuckDbPool::new(cfg.resolved_pool_size())?);

        let (activity, redact_sql, activity_backing) =
            build_activity(cfg.config.observability.as_ref())?;

        // Expose `system.activity` when the table sink is on. The `system`
        // schema is reserved (no source may take the name), mirroring
        // `materialized`.
        if let Some(backing) = &activity_backing {
            let system_schema = Arc::new(datafusion::catalog::MemorySchemaProvider::new());
            let _ = system_schema.register_table(
                "activity".to_string(),
                Arc::new(crate::system_table::ActivityTableProvider::new(
                    backing.clone(),
                )),
            );
            let _ = catalog.register_schema(pawrly_core::SYSTEM_SCHEMA, system_schema);
        }
        let activity_durable = match &activity_backing {
            Some(crate::system_table::ActivityBacking::Durable(store)) => Some(store.clone()),
            _ => None,
        };

        // Build the semantic catalog before the config is consumed into
        // engine-side sources below.
        let semantic_models = cfg
            .config
            .semantic
            .as_ref()
            .map(|s| s.models.clone())
            .unwrap_or_default();
        let semantic = Arc::new(SemanticCatalog::new(semantic_models));

        let inner = Arc::new(LocalEngineInner {
            ctx,
            catalog,
            sources: RwLock::new(HashMap::new()),
            workspace_dir: cfg.workspace_dir.clone(),
            cache,
            semantic,
            refreshers: Mutex::new(HashMap::new()),
            config_path,
            duckdb,
            allow_inline_materialize: cfg.config.defaults.materialize.allow_inline,
            active_queries: Arc::new(AtomicI64::new(0)),
            activity,
            redact_sql,
            activity_durable,
        });

        // Move config into engine-side SourceDefs.
        let engine_sources = cfg.config.into_engine_sources();
        for def in engine_sources {
            register_source(&inner, def).await?;
        }
        // Register semantic pre-aggregations as cached rollup tables (after the
        // base tables they aggregate exist).
        crate::preagg::register_rollups(&inner).await?;
        Ok(Self { inner })
    }

    /// Convenience: load a YAML config from disk and build an engine in one step.
    /// The secret-resolution chain is built from the config's `secrets:` block
    /// (defaulting to the `auto` chain: env, keyring, then a `.env` file).
    pub async fn from_config_file(path: &std::path::Path) -> Result<Self, EngineError> {
        Self::from_config_file_with_home(path, None).await
    }

    /// [`from_config_file`](Self::from_config_file) with an explicit Pawrly
    /// home directory (the CLI threads `--home` / `$PAWRLY_HOME` through here).
    pub async fn from_config_file_with_home(
        path: &std::path::Path,
        home: Option<PathBuf>,
    ) -> Result<Self, EngineError> {
        let cfg =
            pawrly_config::load_auto(path).map_err(|e| EngineError::Internal(e.to_string()))?;
        // `workspace_dir` only anchors relative *source* paths to the config
        // file's directory. The Pawrly data dir is resolved separately from
        // `defaults.cache.storage` / the home (default `~/.pawrly`), not from
        // here.
        let workspace_dir = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        Self::build(
            LocalEngineConfig {
                config: cfg,
                workspace_dir,
                duckdb_pool_size: None,
                home,
            },
            Some(path.to_path_buf()),
        )
        .await
    }

    /// Build an engine with no sources at all (used for `pawrly init` smoke).
    pub async fn empty(workspace_dir: PathBuf) -> Result<Self, EngineError> {
        let cfg = pawrly_config::Config {
            version: 1,
            name: "empty".into(),
            defaults: Default::default(),
            secrets: Vec::new(),
            include: Vec::new(),
            sources: Vec::new(),
            semantic: None,
            observability: None,
        };
        Self::new(LocalEngineConfig {
            config: cfg,
            workspace_dir,
            duckdb_pool_size: None,
            home: None,
        })
        .await
    }

    /// Compile a semantic query to SQL, transparently reading a materialized
    /// rollup when a fresh one covers it. A covering-but-unmaterialized rollup
    /// is built on demand (best-effort); on any miss the base table is used, so
    /// a rollup never changes a result, only how it is computed.
    #[tracing::instrument(name = "pawrly.semantic.compile", skip_all)]
    async fn compile_semantic(&self, q: &SemanticQuery) -> Result<String, EngineError> {
        let started = std::time::Instant::now();
        let sql = if let Some(r) = self.inner.semantic.candidate_rollup(q) {
            let key = TableName::new(r.schema().to_string(), r.table());
            if !self.inner.cache.is_fresh(&key) {
                // Materialize on first use; ignore failure and fall back to base.
                let _ = self.inner.cache.refresh(&key, &self.inner.ctx).await;
            }
            if self.inner.cache.is_fresh(&key) {
                self.inner.semantic.compile_rollup_sql(q, &r)?
            } else {
                self.inner.semantic.compile_sql(q)?
            }
        } else {
            self.inner.semantic.compile_sql(q)?
        };
        pawrly_telemetry::metrics::semantic_compile_duration()
            .record(started.elapsed().as_secs_f64() * 1000.0, &[]);
        Ok(sql)
    }

    /// Produce `(schema, batches)` for a materialize spec. The optional
    /// [`tempfile::NamedTempFile`] (for `Inline`) must outlive the read, so it is
    /// returned to the caller to hold.
    async fn produce_materialize(
        &self,
        spec: &MaterializeSpec,
    ) -> Result<
        (
            SchemaRef,
            Vec<arrow_array::RecordBatch>,
            Option<tempfile::NamedTempFile>,
        ),
        EngineError,
    > {
        match spec {
            MaterializeSpec::Query { sql, params } => {
                let sql = substitute_params(sql, params);
                let df = self
                    .inner
                    .ctx
                    .sql(&sql)
                    .await
                    .map_err(|e| EngineError::InvalidSql(e.to_string()))?;
                // Plan before collect so the schema is known even for 0 rows.
                let schema: SchemaRef = Arc::new(df.schema().as_arrow().clone());
                let batches = df
                    .collect()
                    .await
                    .map_err(|e| EngineError::Internal(format!("materialize collect: {e}")))?;
                Ok((schema, batches, None))
            }
            MaterializeSpec::File { path, format } => {
                // Relative paths resolve against the process cwd (DuckDB's
                // behavior), matching how a shell file argument is read.
                let loc = path.to_string_lossy();
                let fmt = format
                    .or_else(|| pawrly_core::MaterializeFormat::from_path(&loc))
                    .ok_or_else(|| {
                        EngineError::Internal(format!(
                            "could not infer format from `{loc}`; pass an explicit format"
                        ))
                    })?;
                let batches = self.duckdb_scan(&loc, fmt).await?;
                Ok((batches_schema(&batches), batches, None))
            }
            MaterializeSpec::Url { url, format } => {
                // Remote reads go through DuckDB httpfs.
                self.inner.duckdb.ensure_extension("httpfs").await?;
                let fmt = format
                    .or_else(|| pawrly_core::MaterializeFormat::from_path(url))
                    .ok_or_else(|| {
                        EngineError::Internal(format!(
                            "could not infer format from `{url}`; pass an explicit format"
                        ))
                    })?;
                let batches = self.duckdb_scan(url, fmt).await?;
                Ok((batches_schema(&batches), batches, None))
            }
            MaterializeSpec::Inline { bytes, format } => {
                // Stage the bytes in a temp file so the same DuckDB reader path
                // serves them. The handle is returned so it outlives the read.
                let tmp = tempfile::NamedTempFile::new()
                    .map_err(|e| EngineError::Internal(format!("materialize inline tmp: {e}")))?;
                std::fs::write(tmp.path(), bytes)
                    .map_err(|e| EngineError::Internal(format!("materialize inline write: {e}")))?;
                let loc = tmp.path().to_string_lossy().into_owned();
                let batches = self.duckdb_scan(&loc, *format).await?;
                Ok((batches_schema(&batches), batches, Some(tmp)))
            }
        }
    }

    /// Read a file/URL through DuckDB's `read_parquet`/`read_csv`/`read_json`
    /// and return the result as Arrow batches.
    async fn duckdb_scan(
        &self,
        location: &str,
        format: pawrly_core::MaterializeFormat,
    ) -> Result<Vec<arrow_array::RecordBatch>, EngineError> {
        let reader = match format {
            pawrly_core::MaterializeFormat::Parquet => "read_parquet",
            pawrly_core::MaterializeFormat::Csv => "read_csv",
            pawrly_core::MaterializeFormat::Json => "read_json",
        };
        let escaped = location.replace('\'', "''");
        let sql = format!("SELECT * FROM {reader}('{escaped}')");
        self.inner.duckdb.fetch_arrow(&sql).await
    }
}

/// Resolve the per-workspace cache namespace (a single path segment under the
/// shared storage root).
///
/// With an explicit `namespace` set in config, it is sanitized and used as-is
/// (so users can pin a stable id or deliberately share a cache). The workspace
/// rooted at the Pawrly home itself (`$PAWRLY_HOME/pawrly.yaml`) is the
/// *default workspace* and gets the literal namespace `default`. Otherwise a
/// stable id `<dirname>-<hash>` is derived from the canonicalized workspace
/// path, so distinct workspaces never collide on identical `schema.table`
/// names while the same workspace always maps to the same directory.
fn cache_namespace(
    explicit: Option<&str>,
    workspace_dir: &std::path::Path,
    home: Option<&std::path::Path>,
) -> String {
    if let Some(ns) = explicit {
        // Require a real alphanumeric character so a blank or all-whitespace
        // value (which would sanitize to e.g. `---`) falls back to the derived
        // id rather than becoming a meaningless directory name.
        if ns.chars().any(|c| c.is_ascii_alphanumeric()) {
            return sanitize_segment(ns);
        }
    }
    // Canonicalize so `./foo`, `foo`, and `/abs/foo` map to one id. Fall back
    // to the raw path if canonicalization fails (e.g. dir not yet created).
    let canonical =
        std::fs::canonicalize(workspace_dir).unwrap_or_else(|_| workspace_dir.to_path_buf());
    if let Some(h) = home {
        let home_canonical = std::fs::canonicalize(h).unwrap_or_else(|_| h.to_path_buf());
        if canonical == home_canonical {
            return "default".to_string();
        }
    }
    let hash = fnv1a_hex(canonical.as_os_str().as_encoded_bytes());
    let dirname = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .map(sanitize_segment)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "workspace".to_string());
    format!("{dirname}-{hash}")
}

/// Keep a path segment filesystem-safe: alphanumerics, `_`, `-`, `.` pass
/// through; every other character becomes `-`.
fn sanitize_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// FNV-1a 64-bit hash, rendered as 16 lowercase hex chars. Hand-rolled so the
/// on-disk namespace is stable across Rust toolchain versions (unlike
/// `std`'s `DefaultHasher`), which matters for a persistent directory name.
fn fnv1a_hex(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Expand a leading `~` / `~/` in a path against `$HOME`. Any other path is
/// returned unchanged. Used to resolve an explicit `defaults.cache.storage`
/// value like `~/.pawrly/cache` so it lands under `$HOME`, not the workspace.
fn expand_tilde(path: &std::path::Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod cache_path_tests {
    use super::{cache_namespace, fnv1a_hex, sanitize_segment};
    use std::path::Path;

    #[test]
    fn explicit_namespace_is_sanitized_and_used() {
        let ns = cache_namespace(Some("My Cache/v2"), Path::new("/whatever"), None);
        assert_eq!(ns, "My-Cache-v2");
    }

    #[test]
    fn blank_explicit_namespace_falls_back_to_derived() {
        // An all-illegal-to-empty explicit value must not yield an empty segment.
        let ns = cache_namespace(Some("   "), Path::new("/tmp"), None);
        assert!(ns.contains('-') && !ns.starts_with('-'));
    }

    #[test]
    fn derived_namespace_is_stable_and_distinct() {
        // Same path → same id; different paths → different ids.
        let a1 = cache_namespace(None, Path::new("/tmp/ws-a-does-not-exist"), None);
        let a2 = cache_namespace(None, Path::new("/tmp/ws-a-does-not-exist"), None);
        let b = cache_namespace(None, Path::new("/tmp/ws-b-does-not-exist"), None);
        assert_eq!(a1, a2, "same workspace path must map to the same namespace");
        assert_ne!(a1, b, "distinct workspaces must not collide");
        assert!(a1.starts_with("ws-a-does-not-exist-"));
    }

    #[test]
    fn home_workspace_gets_default_namespace() {
        // A workspace rooted at the Pawrly home itself is the default workspace.
        let home = Path::new("/tmp/pawrly-home-does-not-exist");
        let ns = cache_namespace(None, home, Some(home));
        assert_eq!(ns, "default");
        // An explicit namespace still wins over the default-workspace rule.
        let pinned = cache_namespace(Some("pinned"), home, Some(home));
        assert_eq!(pinned, "pinned");
        // Other workspaces are unaffected by the home path.
        let other = cache_namespace(None, Path::new("/tmp/ws-x-does-not-exist"), Some(home));
        assert!(other.starts_with("ws-x-does-not-exist-"));
    }

    #[test]
    fn fnv_is_16_hex_and_deterministic() {
        let h = fnv1a_hex(b"hello");
        assert_eq!(h.len(), 16);
        assert_eq!(h, fnv1a_hex(b"hello"));
        assert_ne!(h, fnv1a_hex(b"world"));
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sanitize_keeps_safe_chars() {
        assert_eq!(sanitize_segment("a_b-c.d1"), "a_b-c.d1");
        assert_eq!(sanitize_segment("a/b\\c d"), "a-b-c-d");
    }
}

async fn register_source(inner: &Arc<LocalEngineInner>, def: SourceDef) -> Result<(), EngineError> {
    let kind = def.kind;
    let name = def.name.clone();

    // `materialized` is reserved for self-backed materialized tables (the
    // namespace catalog's write schema). A source claiming it would collide, so
    // reject it here (the config validator catches the static case too).
    if name == pawrly_core::MATERIALIZED_SCHEMA {
        return Err(EngineError::Internal(format!(
            "source name `{name}` is reserved for materialized tables"
        )));
    }
    if name == pawrly_core::SYSTEM_SCHEMA {
        return Err(EngineError::Internal(format!(
            "source name `{name}` is reserved for engine system tables"
        )));
    }

    // Re-registration path: drop any prior refreshers and tables for this source
    // so a re-scan reflects the current state (new files appear, vanished files
    // disappear) instead of layering on top of stale registrations.
    abort_refreshers(inner, &name);
    let _ = inner.catalog.deregister_schema(&name, true);

    let report = registry::register_source(
        &def,
        &inner.ctx,
        inner.catalog.as_ref(),
        &inner.workspace_dir,
        &inner.duckdb,
    )
    .await
    .map_err(|e| EngineError::SourceRegistration {
        name: name.clone(),
        kind: kind.to_string(),
        source: pawrly_core::SourceError::Other(name.clone(), e.to_string()),
    })?;

    // Wrap each table in CachedTableProvider when the source has cache: != none.
    let mut spawned: Vec<JoinHandle<()>> = Vec::new();
    if def.cache.caches()
        && let Some(schema_provider) = inner.catalog.schema(&name)
    {
        for t in &report.tables {
            let original = match schema_provider.table(&t.name).await {
                Ok(Some(p)) => p,
                _ => continue,
            };
            let wrapped = crate::cache::CachedTableProvider::wrap(
                original,
                pawrly_core::TableName::new(name.clone(), t.name.clone()),
                def.cache.clone(),
                inner.cache.clone(),
            );
            // deregister the original; re-register with the wrapped one.
            let _ = schema_provider.deregister_table(&t.name);
            let _ = schema_provider.register_table(t.name.clone(), wrapped);

            // Background modes get a refresher; `wrap` already registered the
            // inner provider with the cache manager.
            if matches!(
                def.cache,
                CachePolicy::Refresh { .. } | CachePolicy::Cron { .. }
            ) && let Some(handle) = crate::cache::refresher::Spawner::spawn_for(
                &tokio::runtime::Handle::current(),
                TableName::new(name.clone(), t.name.clone()),
                def.cache.clone(),
                inner.cache.clone(),
                inner.ctx.clone(),
            ) {
                spawned.push(handle);
            }
        }
    }
    if !spawned.is_empty() {
        inner.refreshers.lock().insert(name.clone(), spawned);
    }

    let info = SourceInfo {
        name: name.clone(),
        kind,
        status: SourceStatus::Ok,
        status_detail: None,
        sub_kind: registry::sub_kind(&def).map(str::to_string),
        table_count: report.table_count,
        registered_at: Utc::now(),
    };
    inner.sources.write().insert(
        name,
        RegisteredSource {
            info,
            tables: report.tables,
            def,
        },
    );
    Ok(())
}

/// Abort and drop the background refreshers for a source, if any.
fn abort_refreshers(inner: &Arc<LocalEngineInner>, name: &str) {
    if let Some(handles) = inner.refreshers.lock().remove(name) {
        for h in handles {
            h.abort();
        }
    }
}

/// Tear a source down: stop refreshers, drop its schema/tables, forget it.
/// Returns `true` if the source was registered.
fn remove_source_inner(inner: &Arc<LocalEngineInner>, name: &str) -> bool {
    abort_refreshers(inner, name);
    let removed = inner.sources.write().remove(name).is_some();
    if removed {
        let _ = inner.catalog.deregister_schema(name, true);
    }
    removed
}

#[async_trait]
impl EngineService for LocalEngine {
    // Root engine span. `skip_all` keeps SQL text and param values off the span
    // (cardinality + secrets); only low-cardinality attributes are attached.
    #[tracing::instrument(
        name = "pawrly.engine.query",
        skip_all,
        fields(pawrly.engine = "local")
    )]
    async fn query(&self, req: QueryRequest) -> Result<QueryStream, EngineError> {
        let inner = self.inner.clone();
        // Tracks active count + terminal metrics. Dropping it on any `?` error
        // below records `status = error`; on success it moves into the result
        // stream and finalizes when that stream ends.
        let mut guard = crate::stream::QueryGuard::start(inner.active_queries.clone());
        if let Some(actx) = self.activity_context(
            &req.context,
            pawrly_core::activity::Operation::Query,
            Some(&req.sql),
            &req.params,
        ) {
            guard = guard.with_activity(actx);
        }
        // Substitute simple `${param:KEY}` occurrences.
        let sql = substitute_params(&req.sql, &req.params);

        // Inline `-- pawrly: materialize <name>` directive: persist the result,
        // then stream it back. Gated so a `SELECT` can't write to disk unless the
        // workspace opts in.
        if inner.allow_inline_materialize
            && let Some((name, body)) = parse_inline_materialize(&sql)
        {
            self.materialize(
                &name,
                MaterializeSpec::Query {
                    sql: body,
                    params: req.params.clone(),
                },
            )
            .await
            .inspect_err(|e| guard.mark_error(e))?;
            tracing::info!(materialized_as = %format!("materialized.{name}"), "inline materialize");
            let read_sql = format!(
                "SELECT * FROM {}.\"{name}\"",
                pawrly_core::MATERIALIZED_SCHEMA
            );
            let df = inner
                .ctx
                .sql(&read_sql)
                .await
                .map_err(|e| EngineError::Internal(format!("datafusion: {e}")))
                .inspect_err(|e| guard.mark_error(e))?;
            let stream = df
                .execute_stream()
                .await
                .map_err(|e| EngineError::Internal(format!("datafusion: {e}")))
                .inspect_err(|e| guard.mark_error(e))?;
            return Ok(crate::stream::adapt_instrumented(stream, guard));
        }

        let df = inner
            .ctx
            .sql(&sql)
            .await
            .map_err(|e| EngineError::InvalidSql(e.to_string()))
            .inspect_err(|e| guard.mark_error(e))?;
        let stream = df
            .execute_stream()
            .await
            .map_err(|e| EngineError::Internal(format!("datafusion: {e}")))
            .inspect_err(|e| guard.mark_error(e))?;
        Ok(crate::stream::adapt_instrumented(stream, guard))
    }

    #[tracing::instrument(name = "pawrly.engine.explain", skip_all, fields(pawrly.engine = "local"))]
    async fn explain(&self, sql: &str, _analyze: bool) -> Result<String, EngineError> {
        let df = self
            .inner
            .ctx
            .sql(sql)
            .await
            .map_err(|e| EngineError::InvalidSql(e.to_string()))?;
        let plan = df.logical_plan().display_indent_schema().to_string();
        Ok(plan)
    }

    async fn cancel(&self, _query_id: &QueryId) -> Result<bool, EngineError> {
        // No in-flight tracking; cancellation is not yet supported.
        Ok(false)
    }

    async fn list_sources(&self) -> Result<Vec<SourceInfo>, EngineError> {
        Ok(self
            .inner
            .sources
            .read()
            .values()
            .map(|s| s.info.clone())
            .collect())
    }

    async fn list_tables(
        &self,
        filter: Option<TableFilter>,
    ) -> Result<Vec<TableInfo>, EngineError> {
        let sources = self.inner.sources.read();
        let mut out = Vec::new();
        for src in sources.values() {
            if let Some(f) = &filter {
                if let Some(want) = &f.source {
                    if &src.info.name != want {
                        continue;
                    }
                }
            }
            for t in &src.tables {
                out.push(TableInfo {
                    name: TableName::new(src.info.name.clone(), t.name.clone()),
                    kind: src.info.kind,
                    description: t.description.clone(),
                    row_count_estimate: None,
                    cached: false,
                    required_filters: t.required_filters.clone(),
                });
            }
        }
        Ok(out)
    }

    async fn describe_table(&self, name: &TableName) -> Result<TableDescription, EngineError> {
        // Use DataFusion's catalog to look up the schema.
        let schema = self
            .inner
            .catalog
            .schema(&name.schema)
            .ok_or_else(|| EngineError::UnknownTable(name.to_string()))?;
        let table = schema
            .table(&name.table)
            .await
            .map_err(|e| EngineError::Internal(format!("datafusion: {e}")))?
            .ok_or_else(|| EngineError::UnknownTable(name.to_string()))?;
        let arrow_schema = table.schema();

        let columns: Vec<ColumnSpec> = arrow_schema
            .fields()
            .iter()
            .map(|f| ColumnSpec {
                name: f.name().clone(),
                data_type: format!("{:?}", f.data_type()),
                nullable: f.is_nullable(),
                description: None,
                is_filter_pushable: false,
                is_required_filter: false,
            })
            .collect();

        let (kind, description, required_filters, wiki, examples) = {
            let sources = self.inner.sources.read();
            let src = sources
                .get(&name.schema)
                .ok_or_else(|| EngineError::UnknownTable(name.to_string()))?;
            let summary = src.tables.iter().find(|t| t.name == name.table);
            // Source-level notes apply to every table; prepend them to the
            // table's own wiki when both exist.
            let wiki = match (src.def.wiki.clone(), summary.and_then(|t| t.wiki.clone())) {
                (Some(s), Some(t)) => Some(format!("{s}\n\n{t}")),
                (s, t) => s.or(t),
            };
            let qualified = name.to_string();
            let examples: Vec<String> = src
                .def
                .examples
                .iter()
                .filter(|sql| sql.contains(&qualified))
                .cloned()
                .collect();
            (
                src.info.kind,
                summary.and_then(|t| t.description.clone()),
                summary
                    .map(|t| t.required_filters.clone())
                    .unwrap_or_default(),
                wiki,
                examples,
            )
        };

        Ok(TableDescription {
            table: TableInfo {
                name: name.clone(),
                kind,
                description,
                row_count_estimate: None,
                cached: false,
                required_filters,
            },
            columns,
            pushable_filter_columns: Vec::new(),
            examples,
            wiki,
        })
    }

    async fn schema_snapshot(
        &self,
        sources: Option<Vec<String>>,
        _compact: bool,
    ) -> Result<CatalogSnapshot, EngineError> {
        let registered = self.inner.sources.read();
        let mut schemas = Vec::new();
        for src in registered.values() {
            if let Some(filter) = &sources {
                if !filter.contains(&src.info.name) {
                    continue;
                }
            }
            schemas.push(SchemaSummary {
                name: src.info.name.clone(),
                kind: src.info.kind,
                tables: src
                    .tables
                    .iter()
                    .map(|t| TableSummary {
                        name: t.name.clone(),
                        columns: String::new(),
                        required_filters: t.required_filters.clone(),
                    })
                    .collect(),
            });
        }
        Ok(CatalogSnapshot { schemas })
    }

    async fn refresh_catalog(
        &self,
        source: Option<&str>,
    ) -> Result<RefreshCatalogOutcome, EngineError> {
        // Snapshot the defs to refresh (cloned so we don't hold the lock across
        // the await in `register_source`).
        let defs: Vec<SourceDef> = {
            let registered = self.inner.sources.read();
            match source {
                Some(name) => {
                    let s = registered.get(name).ok_or_else(|| {
                        EngineError::UnknownTable(format!("source `{name}` is not registered"))
                    })?;
                    vec![s.def.clone()]
                }
                None => registered.values().map(|s| s.def.clone()).collect(),
            }
        };

        // Re-registering re-enumerates file globs and re-infers schemas, so new
        // files are picked up and removed files drop out.
        let names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
        for def in defs {
            register_source(&self.inner, def).await?;
        }

        let registered = self.inner.sources.read();
        let tables_discovered = names
            .iter()
            .filter_map(|n| registered.get(n))
            .map(|s| s.tables.len() as u64)
            .sum();
        Ok(RefreshCatalogOutcome {
            sources_refreshed: names.len() as u64,
            tables_discovered,
        })
    }

    async fn cache_entries(&self) -> Result<Vec<CacheEntryInfo>, EngineError> {
        Ok(self.inner.cache.list())
    }

    async fn refresh_table(&self, name: &TableName) -> Result<RefreshOutcome, EngineError> {
        // A materialized table has no live inner provider to re-scan — re-run its
        // stored origin spec (re-execute the query / re-read the file or URL) and
        // overwrite the pinned Parquet.
        if name.schema == pawrly_core::MATERIALIZED_SCHEMA {
            let spec = self
                .inner
                .cache
                .materialized_spec(&name.table)
                .ok_or_else(|| EngineError::UnknownTable(name.to_string()))?;
            let started = std::time::Instant::now();
            let (schema, batches, _tmp) = self.produce_materialize(&spec).await?;
            let entry = self
                .inner
                .cache
                .materialize(&name.table, schema, &batches, spec)
                .map_err(|e| EngineError::Internal(format!("materialize refresh: {e}")))?;
            return Ok(RefreshOutcome {
                table: name.clone(),
                rows_written: entry.row_count,
                size_bytes: entry.size_bytes,
                elapsed: started.elapsed(),
                expires_at: None,
            });
        }
        self.inner.cache.refresh(name, &self.inner.ctx).await
    }

    async fn invalidate_cache(&self, name: &TableName) -> Result<bool, EngineError> {
        self.inner.cache.invalidate(name)
    }

    async fn vacuum_cache(&self) -> Result<VacuumReport, EngineError> {
        self.inner.cache.vacuum()
    }

    #[tracing::instrument(
        name = "pawrly.engine.materialize",
        skip_all,
        fields(pawrly.engine = "local", pawrly.table = %name)
    )]
    async fn materialize(
        &self,
        name: &str,
        spec: MaterializeSpec,
    ) -> Result<MaterializeOutcome, EngineError> {
        validate_materialized_name(name)?;

        // Every origin reduces to "produce Arrow batches + a schema". `_tmp`
        // keeps an Inline spec's backing file alive until the read completes.
        let (schema, batches, _tmp) = self.produce_materialize(&spec).await?;

        let entry = self
            .inner
            .cache
            .materialize(name, schema, &batches, spec)
            .map_err(|e| EngineError::Internal(format!("materialize write: {e}")))?;

        Ok(MaterializeOutcome {
            name: TableName::new(pawrly_core::MATERIALIZED_SCHEMA, name),
            file_path: entry.file_path,
            row_count: entry.row_count,
            size_bytes: entry.size_bytes,
        })
    }

    async fn drop_materialized(&self, name: &str) -> Result<bool, EngineError> {
        self.inner.cache.drop_materialized(name)
    }

    async fn add_source(&self, def: SourceDef) -> Result<SourceInfo, EngineError> {
        register_source(&self.inner, def.clone()).await?;
        let info = self
            .inner
            .sources
            .read()
            .get(&def.name)
            .map(|s| s.info.clone())
            .ok_or_else(|| EngineError::Internal("source vanished after register".into()))?;
        Ok(info)
    }

    async fn remove_source(&self, name: &str) -> Result<bool, EngineError> {
        Ok(remove_source_inner(&self.inner, name))
    }

    async fn test_source(&self, name: &str) -> Result<SourceTestReport, EngineError> {
        let exists = self.inner.sources.read().contains_key(name);
        Ok(SourceTestReport {
            name: name.to_string(),
            ok: exists,
            latency: std::time::Duration::from_millis(0),
            detail: if exists {
                Some("registered".into())
            } else {
                Some("not registered".into())
            },
        })
    }

    async fn reload_config(&self) -> Result<ReloadReport, EngineError> {
        let Some(path) = self.inner.config_path.clone() else {
            return Err(EngineError::Internal(
                "reload_config requires an engine built from a config file".into(),
            ));
        };

        let cfg =
            pawrly_config::load_auto(&path).map_err(|e| EngineError::Internal(e.to_string()))?;
        let new_defs = cfg.into_engine_sources();

        // Snapshot current sources as (name -> serialized def) for diffing.
        let current: HashMap<String, serde_json::Value> = {
            let registered = self.inner.sources.read();
            registered
                .iter()
                .map(|(n, s)| {
                    (
                        n.clone(),
                        serde_json::to_value(&s.def).unwrap_or(serde_json::Value::Null),
                    )
                })
                .collect()
        };

        let mut report = ReloadReport::default();
        let mut seen = std::collections::HashSet::new();
        for def in new_defs {
            let new_json = serde_json::to_value(&def).unwrap_or(serde_json::Value::Null);
            seen.insert(def.name.clone());
            match current.get(&def.name) {
                None => {
                    register_source(&self.inner, def).await?;
                    report.sources_added += 1;
                }
                Some(old_json) if *old_json != new_json => {
                    register_source(&self.inner, def).await?;
                    report.sources_changed += 1;
                }
                Some(_) => {} // unchanged
            }
        }

        for name in current.keys().filter(|n| !seen.contains(*n)) {
            if remove_source_inner(&self.inner, name) {
                report.sources_removed += 1;
            }
        }

        Ok(report)
    }

    async fn list_semantic_models(&self) -> Result<Vec<SemanticModelInfo>, EngineError> {
        Ok(self.inner.semantic.list())
    }

    async fn describe_semantic_model(
        &self,
        name: &str,
    ) -> Result<SemanticModelDescription, EngineError> {
        self.inner
            .semantic
            .describe(name)
            .ok_or_else(|| EngineError::SemanticPlan(format!("unknown semantic model `{name}`")))
    }

    #[tracing::instrument(name = "pawrly.engine.semantic_query", skip_all, fields(pawrly.engine = "local"))]
    async fn semantic_query(&self, q: SemanticQuery) -> Result<QueryStream, EngineError> {
        let mut guard = crate::stream::QueryGuard::start(self.inner.active_queries.clone());
        // SemanticQuery carries no RequestContext, so activity attribution
        // defaults to in-process here.
        if let Some(actx) = self.activity_context(
            &pawrly_core::activity::RequestContext::default(),
            pawrly_core::activity::Operation::SemanticQuery,
            None,
            &q.params,
        ) {
            guard = guard.with_activity(actx);
        }
        // Compile to SQL — reading a materialized rollup when one covers the
        // query — and execute through the same DataFusion path as `query`.
        let sql = self
            .compile_semantic(&q)
            .await
            .inspect_err(|e| guard.mark_error(e))?;
        // Record the compiled SQL on the activity record now that it's known,
        // so `system.activity` shows what a semantic_query actually executed.
        guard.set_activity_sql(self.redact_activity_sql(&sql));
        let df = self
            .inner
            .ctx
            .sql(&sql)
            .await
            .map_err(|e| EngineError::InvalidSql(e.to_string()))
            .inspect_err(|e| guard.mark_error(e))?;
        let stream = df
            .execute_stream()
            .await
            .map_err(|e| EngineError::Internal(format!("datafusion: {e}")))
            .inspect_err(|e| guard.mark_error(e))?;
        Ok(crate::stream::adapt_instrumented(stream, guard))
    }

    async fn health(&self) -> Result<HealthReport, EngineError> {
        let sources = self.inner.sources.read();
        Ok(HealthReport {
            ok: true,
            version: env!("CARGO_PKG_VERSION").into(),
            active_queries: self.inner.active_queries.load(Ordering::Relaxed).max(0) as u64,
            sources_ok: sources
                .values()
                .filter(|s| matches!(s.info.status, SourceStatus::Ok))
                .count() as u64,
            sources_unavailable: sources
                .values()
                .filter(|s| matches!(s.info.status, SourceStatus::Unavailable))
                .count() as u64,
        })
    }

    async fn shutdown(&self) -> Result<(), EngineError> {
        let drained: Vec<JoinHandle<()>> = self
            .inner
            .refreshers
            .lock()
            .drain()
            .flat_map(|(_, handles)| handles)
            .collect();
        for handle in drained {
            handle.abort();
        }
        Ok(())
    }
}

/// Build the activity sink, redaction policy, and (when the `table` sink is on)
/// the backing for `system.activity` — the durable on-disk store when
/// `activity.store` is set, otherwise the in-memory ring. Disabled (a no-op
/// sink) unless `activity.enabled` and a supported sink is configured.
fn build_activity(
    obs: Option<&pawrly_config::ObservabilityConfig>,
) -> Result<
    (
        crate::activity::ActivitySink,
        crate::redact::RedactMode,
        Option<crate::system_table::ActivityBacking>,
    ),
    EngineError,
> {
    use crate::system_table::ActivityBacking;
    use pawrly_config::{ActivitySinkKind, RedactSql};

    let Some(act) = obs.map(|o| &o.activity).filter(|a| a.enabled) else {
        return Ok((
            crate::activity::ActivitySink::disabled(),
            crate::redact::RedactMode::Off,
            None,
        ));
    };
    let redact = match act.redact_sql {
        RedactSql::Off => crate::redact::RedactMode::Off,
        RedactSql::Literals => crate::redact::RedactMode::Literals,
        RedactSql::Tables => crate::redact::RedactMode::TablesOnly,
    };

    let mut recorders: Vec<Arc<dyn pawrly_core::activity::ActivityRecorder>> = Vec::new();
    if act.sinks.contains(&ActivitySinkKind::Tracing) {
        recorders.push(Arc::new(crate::activity::TracingRecorder));
    }
    let mut backing = None;
    if act.sinks.contains(&ActivitySinkKind::Table) {
        let b = match &act.store {
            Some(dir) => {
                let store = crate::durable_activity::DurableActivityStore::open(
                    expand_tilde(dir),
                    act.partition_hours,
                    act.flush_threshold,
                    act.flush_interval,
                    act.retention,
                )?;
                recorders.push(Arc::new(store.clone()));
                ActivityBacking::Durable(store)
            }
            None => {
                let ring = crate::system_table::ActivityStore::new(act.ring_capacity);
                recorders.push(Arc::new(ring.clone()));
                ActivityBacking::Ring(ring)
            }
        };
        backing = Some(b);
    }

    let sink = if recorders.is_empty() {
        crate::activity::ActivitySink::disabled()
    } else {
        crate::activity::ActivitySink::spawn(
            Arc::new(crate::activity::MultiRecorder(recorders)),
            act.ring_capacity,
        )
    };
    Ok((sink, redact, backing))
}

/// Extract the 32-hex trace-id from a W3C `traceparent` (`00-<trace>-<span>-..`).
fn trace_id_from_traceparent(tp: &str) -> Option<String> {
    let trace = tp.split('-').nth(1)?;
    (trace.len() == 32 && trace.bytes().all(|b| b.is_ascii_hexdigit())).then(|| trace.to_string())
}

fn substitute_params(sql: &str, params: &HashMap<String, String>) -> String {
    let mut out = sql.to_string();
    for (k, v) in params {
        let needle = format!("${{param:{k}}}");
        out = out.replace(&needle, v);
    }
    out
}

/// Parse a leading `-- pawrly: materialize <name>` directive, returning the
/// target name and the query body with that line removed. Recognized only in the
/// leading comment block (before the first non-comment token), so it can never
/// fire from a comment inside a string literal or further down the query.
fn parse_inline_materialize(sql: &str) -> Option<(String, String)> {
    for (i, raw) in sql.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let Some(comment) = line.strip_prefix("--") else {
            // First real token reached with no directive.
            return None;
        };
        if let Some(rest) = comment.trim().strip_prefix("pawrly:")
            && let Some(args) = rest.trim().strip_prefix("materialize")
        {
            let name = args.split_whitespace().next()?.to_string();
            let body: String = sql
                .lines()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, l)| l)
                .collect::<Vec<_>>()
                .join("\n");
            return Some((name, body));
        }
        // A different leading comment — keep scanning.
    }
    None
}

/// Schema of the first batch, or an empty schema when there are no batches
/// (e.g. materializing an empty file) — enough to write a valid empty Parquet.
fn batches_schema(batches: &[arrow_array::RecordBatch]) -> SchemaRef {
    batches
        .first()
        .map(arrow_array::RecordBatch::schema)
        .unwrap_or_else(|| Arc::new(arrow_schema::Schema::empty()))
}

/// A materialized table name becomes a single SQL identifier under the
/// `materialized` schema and a single path segment on disk, so it must be a
/// plain identifier — no dots (would imply qualification), path separators, or
/// surrounding whitespace.
fn validate_materialized_name(name: &str) -> Result<(), EngineError> {
    let bad = name.is_empty()
        || name.trim() != name
        || name.contains(|c: char| c == '.' || c == '/' || c == '\\' || c.is_whitespace());
    if bad {
        return Err(EngineError::Internal(format!(
            "invalid materialized table name `{name}`: use a plain identifier (no dots, slashes, or spaces)"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_core::EngineServiceExt;

    #[tokio::test]
    async fn empty_engine_runs_a_literal_query() {
        let dir = std::env::temp_dir();
        let engine = LocalEngine::empty(dir).await.unwrap();
        let batches = engine.query_collect("SELECT 1 AS x").await.unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
        assert_eq!(batches[0].schema().field(0).name(), "x");
    }
}
