//! `LocalEngine` — in-process implementation of `EngineService`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use arrow_schema::SchemaRef;
use async_trait::async_trait;
use chrono::Utc;
use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider, SchemaProvider};
use datafusion::execution::context::SessionContext;
use parking_lot::{Mutex, RwLock};
use pawrly_core::semantic::{SemanticModelDescription, SemanticModelInfo, SemanticQuery};
use pawrly_core::{
    CacheEntryInfo, CachePolicy, CatalogSnapshot, ColumnSpec, EngineError, EngineService,
    HealthReport, MaterializeOutcome, MaterializeSpec, QueryHandle, QueryId, QueryRequest,
    RefreshCatalogOutcome, RefreshOutcome, ReloadReport, SchemaSummary, SourceDef, SourceInfo,
    SourceStatus, SourceTestReport, TableDescription, TableFilter, TableInfo, TableName,
    TableSummary, VacuumReport,
};
use pawrly_semantic::SemanticCatalog;
use tokio::task::JoinHandle;

use crate::cache::CacheManager;
use crate::duckdb_pool::DuckDbPool;
use crate::registry;

pub(crate) const PAWRLY_CATALOG: &str = "pawrly";

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
    /// Table-valued function registry (builtins + declared). Same `RwLock`
    /// pattern as `sources`.
    functions: RwLock<crate::functions::FunctionRegistry>,
    workspace_dir: PathBuf,
    /// The default namespace's manager, used directly by the read-through
    /// cache paths; the materialize verbs resolve through `namespaces`.
    pub(crate) cache: Arc<CacheManager>,
    /// Per-call materialize namespaces, shared with the session's catalog
    /// list so a namespaced table is queryable the moment it exists.
    pub(crate) namespaces: Arc<crate::namespace::NamespaceRegistry>,
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
    /// In-flight query cancel registry. Populated at query start; entries removed
    /// by the `QueryGuard` when the stream ends. `cancel(id)` sets the flag.
    cancellations: crate::stream::CancelRegistry,
    /// Activity-log sink (disabled unless `observability.activity` is enabled).
    activity: crate::activity::ActivitySink,
    /// SQL redaction policy for activity records.
    redact_sql: crate::redact::RedactMode,
    /// Durable activity store, when configured. Held so its buffered tail is
    /// flushed to disk when the engine is torn down.
    activity_durable: Option<crate::durable_activity::DurableActivityStore>,
    /// Process-wide store for dynamic `${var:}` variables (OAuth-minted tokens).
    /// Rebuilt on `reload_config` (so new specs take effect); the live store's
    /// in-memory token cache lasts as long as the engine does.
    pub(crate) variables: RwLock<Arc<dyn pawrly_secrets::VariableStore>>,
    /// Per-source `NAME → VarId` maps, threaded to each source registrar so it
    /// can resolve its dynamic `${var:}` placeholders. Keyed by source name.
    dynamic_bindings: RwLock<HashMap<String, HashMap<String, pawrly_core::VarId>>>,
    /// Persisted refresh-token store backing the interactive grants. Shared by
    /// the variable store and reused when it is rebuilt on reload.
    tokens: Arc<dyn pawrly_secrets::VariableValueStore>,
    /// Where OIDC discovery documents are cached; reused when the variable store
    /// is rebuilt on reload.
    discovery_cache_dir: Option<std::path::PathBuf>,
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
    /// Live connection handle (http/mcp), so attached functions can share it.
    function_handle: crate::functions::SourceHandle,
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
        Self::build(cfg, None, None).await
    }

    /// Like [`Self::new`], but with an explicit token store — for tests and
    /// embedders that need a deterministic or non-keyring backend.
    pub async fn new_with_token_store(
        cfg: LocalEngineConfig,
        tokens: Arc<dyn pawrly_secrets::VariableValueStore>,
    ) -> Result<Self, EngineError> {
        Self::build(cfg, None, Some(tokens)).await
    }

    async fn build(
        cfg: LocalEngineConfig,
        config_path: Option<PathBuf>,
        tokens_override: Option<Arc<dyn pawrly_secrets::VariableValueStore>>,
    ) -> Result<Self, EngineError> {
        use datafusion::execution::config::SessionConfig;
        use datafusion::execution::session_state::SessionStateBuilder;

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
        let namespaces = Arc::new(crate::namespace::NamespaceRegistry::new(
            storage,
            namespace.clone(),
            cache.clone(),
        ));

        let session_config = SessionConfig::new()
            .with_default_catalog_and_schema(PAWRLY_CATALOG, "default")
            .with_create_default_catalog_and_schema(false)
            .with_information_schema(true);
        // Register the dependent-join rule (last, after the built-in physical
        // rules) so a required-param HTTP table can be driven by another table's
        // ids — e.g. a ranked-id list joined to a get-by-id detail endpoint.
        let session_state = SessionStateBuilder::new()
            .with_config(session_config)
            .with_default_features()
            .with_catalog_list(Arc::new(crate::namespace::DynamicNamespaceCatalogs::new(
                namespaces.clone(),
            )))
            .with_physical_optimizer_rule(Arc::new(pawrly_sources_http::DependentJoinRule::new()))
            .build();
        let mut ctx = SessionContext::new_with_state(session_state);
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

        let activity_durable = match &activity_backing {
            Some(crate::system_table::ActivityBacking::Durable(store)) => Some(store.clone()),
            _ => None,
        };

        // Build the semantic catalog before the config is consumed into
        // engine-side sources below.
        let (semantic_models, semantic_metrics, time_spine) = cfg
            .config
            .semantic
            .as_ref()
            .map(|s| (s.models.clone(), s.metrics.clone(), s.time_spine.clone()))
            .unwrap_or_default();
        let semantic = Arc::new(
            SemanticCatalog::new_with_metrics(semantic_models, semantic_metrics)
                .with_time_spine(time_spine),
        );

        // Build the dynamic-variable store + per-source binding maps while the
        // config is still intact (before `into_engine_sources` consumes it).
        // Refresh tokens for interactive grants persist via the OS keyring (or an
        // encrypted-file fallback), keyed under the Pawrly home.
        let tokens: Arc<dyn pawrly_secrets::VariableValueStore> = match tokens_override {
            Some(tokens) => tokens,
            None => match pawrly_core::resolve_home(cfg.home.as_deref()) {
                Some(home) => Arc::new(pawrly_secrets::VariableTokenStore::new(
                    home.join("variables"),
                )),
                None => Arc::new(pawrly_secrets::NoopTokenStore),
            },
        };
        let discovery_cache_dir = home.as_ref().map(|h| h.join("cache"));
        let variables: Arc<dyn pawrly_secrets::VariableStore> = Arc::new(
            pawrly_secrets::RuntimeVariableStore::with_tokens(
                cfg.config.dynamic_specs(),
                tokens.clone(),
            )
            .with_cache_dir(discovery_cache_dir.clone()),
        );
        let dynamic_bindings = cfg.config.dynamic_bindings_by_source();

        // The reserved `system` schema (no source may take the name): expose
        // `system.activity` (when the activity sink is on) and `system.variables`
        // (declared variables + connection state).
        {
            let system_schema = Arc::new(datafusion::catalog::MemorySchemaProvider::new());
            let mut any = false;
            if let Some(backing) = &activity_backing {
                let _ = system_schema.register_table(
                    "activity".to_string(),
                    Arc::new(crate::system_table::ActivityTableProvider::new(
                        backing.clone(),
                    )),
                );
                any = true;
            }
            // `available` is resolve-accurate: probe the configured secret chain
            // for static secrets (env → keyring → file), not just the env var.
            let table_secrets: Box<dyn pawrly_secrets::SecretStore> =
                pawrly_config::build_store(&cfg.config.secrets, &cfg.workspace_dir)
                    .map(|s| Box::new(s) as Box<dyn pawrly_secrets::SecretStore>)
                    .unwrap_or_else(|_| {
                        Box::new(pawrly_secrets::StaticStore::new())
                            as Box<dyn pawrly_secrets::SecretStore>
                    });
            if let Some(table) = crate::system_variables::build_variables_table(
                &cfg.config,
                tokens.as_ref(),
                table_secrets.as_ref(),
            ) {
                let _ = system_schema.register_table("variables".to_string(), Arc::new(table));
                any = true;
            }
            if any {
                let _ = catalog.register_schema(pawrly_core::SYSTEM_SCHEMA, system_schema);
            }
        }

        let inner = Arc::new(LocalEngineInner {
            ctx,
            catalog,
            sources: RwLock::new(HashMap::new()),
            functions: RwLock::new(crate::functions::FunctionRegistry::default()),
            workspace_dir: cfg.workspace_dir.clone(),
            cache,
            namespaces,
            semantic,
            refreshers: Mutex::new(HashMap::new()),
            config_path,
            duckdb,
            allow_inline_materialize: cfg.config.defaults.materialize.allow_inline,
            active_queries: Arc::new(AtomicI64::new(0)),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            activity,
            redact_sql,
            activity_durable,
            variables: RwLock::new(variables),
            dynamic_bindings: RwLock::new(dynamic_bindings),
            tokens,
            discovery_cache_dir,
        });

        // Resolve declared functions while the config is still intact (their
        // connection configs are cloned from the sources), then consume it.
        let function_defs = cfg.config.engine_functions();

        // Move config into engine-side SourceDefs.
        let engine_sources = cfg.config.into_engine_sources();
        for def in engine_sources {
            register_source(&inner, def).await?;
        }

        // Register table-valued functions: builtins first, then declared.
        register_functions(&inner, function_defs).await?;

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
        // Build the variable value store from the resolved home so a static
        // secret set via `pawrly source connect` resolves at load (stored-wins)
        // instead of hard-failing as an unresolved `${var:}`. The same store is
        // reused as the engine's runtime token store below.
        let tokens: Option<Arc<dyn pawrly_secrets::VariableValueStore>> =
            pawrly_core::resolve_home(home.as_deref()).map(|h| {
                Arc::new(pawrly_secrets::VariableTokenStore::new(h.join("variables")))
                    as Arc<dyn pawrly_secrets::VariableValueStore>
            });
        let cfg = pawrly_config::load_auto_with_vars(path, tokens.as_deref())
            .map_err(|e| EngineError::Internal(e.to_string()))?;
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
            tokens,
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
            variables: Default::default(),
            sources: Vec::new(),
            functions: Vec::new(),
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
                let sql = {
                    let reg = self.inner.functions.read();
                    crate::functions::rewrite_function_calls(&sql, &reg)?
                };
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
pub(crate) fn sanitize_segment(s: &str) -> String {
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

    // Snapshot the store handle + this source's binding map before any await.
    let variables = inner.variables.read().clone();
    let dynamic = inner
        .dynamic_bindings
        .read()
        .get(&name)
        .cloned()
        .unwrap_or_default();

    let report = registry::register_source(
        &def,
        &inner.ctx,
        inner.catalog.as_ref(),
        &inner.workspace_dir,
        &inner.duckdb,
        &variables,
        dynamic,
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
            function_handle: report.function_handle,
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

/// Tear a source down: stop refreshers, drop its schema/tables, forget it, and
/// drop any functions attached to it. Returns `true` if the source was
/// registered.
fn remove_source_inner(inner: &Arc<LocalEngineInner>, name: &str) -> bool {
    abort_refreshers(inner, name);
    let removed = inner.sources.write().remove(name).is_some();
    if removed {
        let _ = inner.catalog.deregister_schema(name, true);
    }
    // Drop the source's attached functions and their UDTFs (no-op if none).
    let mangled = inner.functions.write().remove_by_source(name);
    for m in mangled {
        inner.ctx.deregister_udtf(&m);
    }
    removed
}

/// Register builtins first, then declared functions, on both the UDTF catalog
/// and the function registry.
async fn register_functions(
    inner: &Arc<LocalEngineInner>,
    defs: Vec<pawrly_core::FunctionDef>,
) -> Result<(), EngineError> {
    for def in pawrly_core::function::builtins() {
        register_one_function(inner, def, crate::functions::SourceHandle::None).await?;
    }
    for def in defs {
        // An attached function inherits its parent source's live handle.
        let handle = def
            .source
            .as_deref()
            .and_then(|src| {
                inner
                    .sources
                    .read()
                    .get(src)
                    .map(|s| s.function_handle.clone())
            })
            .unwrap_or_default();
        register_one_function(inner, def, handle).await?;
    }
    Ok(())
}

/// Build one function's executor, register its mangled UDTF, and insert it into
/// the registry. DataFusion overwrites a UDTF on re-registration, which is
/// exactly right for config reload.
async fn register_one_function(
    inner: &Arc<LocalEngineInner>,
    def: pawrly_core::FunctionDef,
    handle: crate::functions::SourceHandle,
) -> Result<(), EngineError> {
    let registered =
        crate::functions::build_registered_function(def, handle, &inner.workspace_dir).await?;
    inner.ctx.register_udtf(
        &registered.mangled,
        Arc::new(crate::functions::PawrlyFunctionUdtf {
            func: registered.clone(),
        }),
    );
    inner.functions.write().insert(registered);
    Ok(())
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
    async fn query(&self, req: QueryRequest) -> Result<QueryHandle, EngineError> {
        let inner = self.inner.clone();
        // Register a cancel flag so `cancel(id)` can find this query; the guard
        // removes the entry when the stream ends.
        let query_id = QueryId::new(uuid::Uuid::new_v4().to_string());
        let cancel_flag: crate::stream::CancelFlag = Arc::new(AtomicBool::new(false));
        inner
            .cancellations
            .lock()
            .insert(query_id.clone(), cancel_flag.clone());
        let completion = Arc::new(std::sync::OnceLock::new());
        // The guard tracks active count, terminal metrics, completion, and
        // registry cleanup: dropping it on any `?` error records `status = error`;
        // on success it moves into the stream and finalizes when the stream ends.
        let mut guard = crate::stream::QueryGuard::start(inner.active_queries.clone())
            .with_cancel(query_id.clone(), inner.cancellations.clone())
            .with_completion(completion.clone());
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
                None,
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
            return Ok(QueryHandle::new(
                query_id,
                crate::stream::adapt_instrumented(stream, guard, Some(cancel_flag)),
                completion,
            ));
        }

        // Rewrite namespaced function calls (`ns.fn(...)`) to their UDTF names
        // before planning. Placed after the inline-materialize check, whose
        // directive comment the rewrite's AST round-trip would strip; that path
        // rewrites its own body inside `materialize`.
        let sql = {
            let reg = inner.functions.read();
            crate::functions::rewrite_function_calls(&sql, &reg)
                .inspect_err(|e| guard.mark_error(e))?
        };

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
        Ok(QueryHandle::new(
            query_id,
            crate::stream::adapt_instrumented(stream, guard, Some(cancel_flag)),
            completion,
        ))
    }

    #[tracing::instrument(name = "pawrly.engine.explain", skip_all, fields(pawrly.engine = "local"))]
    async fn explain(&self, sql: &str, _analyze: bool) -> Result<String, EngineError> {
        let sql = {
            let reg = self.inner.functions.read();
            crate::functions::rewrite_function_calls(sql, &reg)?
        };
        let df = self
            .inner
            .ctx
            .sql(&sql)
            .await
            .map_err(|e| EngineError::InvalidSql(e.to_string()))?;
        let plan = df.logical_plan().display_indent_schema().to_string();
        Ok(plan)
    }

    async fn cancel(&self, query_id: &QueryId) -> Result<bool, EngineError> {
        // Set the query's cancel flag; the stream observes it and ends with
        // `Cancelled`. The entry is removed by the stream's guard, not here.
        match self.inner.cancellations.lock().get(query_id) {
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                Ok(true)
            }
            None => Ok(false),
        }
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

    async fn cache_entries(
        &self,
        namespace: Option<&str>,
    ) -> Result<Vec<CacheEntryInfo>, EngineError> {
        Ok(self
            .inner
            .namespaces
            .for_read(namespace)?
            .map(|cache| cache.list())
            .unwrap_or_default())
    }

    async fn refresh_table(
        &self,
        name: &TableName,
        namespace: Option<&str>,
    ) -> Result<RefreshOutcome, EngineError> {
        // A materialized table has no live inner provider to re-scan — re-run its
        // stored origin spec (re-execute the query / re-read the file or URL) and
        // overwrite the pinned Parquet.
        if name.schema == pawrly_core::MATERIALIZED_SCHEMA {
            let cache = self
                .inner
                .namespaces
                .for_read(namespace)?
                .ok_or_else(|| EngineError::UnknownTable(name.to_string()))?;
            let spec = cache
                .materialized_spec(&name.table)
                .ok_or_else(|| EngineError::UnknownTable(name.to_string()))?;
            let started = std::time::Instant::now();
            let (schema, batches, _tmp) = self.produce_materialize(&spec).await?;
            let entry = cache
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
        if namespace.is_some() {
            return Err(EngineError::Internal(format!(
                "`{name}` is not a materialized table; only `materialized.<name>` \
                 can be refreshed in a namespace"
            )));
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
        fields(
            pawrly.engine = "local",
            pawrly.table = %name,
            pawrly.namespace = namespace.unwrap_or_default()
        )
    )]
    async fn materialize(
        &self,
        name: &str,
        spec: MaterializeSpec,
        namespace: Option<&str>,
    ) -> Result<MaterializeOutcome, EngineError> {
        validate_materialized_name(name)?;
        let cache = self.inner.namespaces.for_write(namespace)?;

        // Every origin reduces to "produce Arrow batches + a schema". `_tmp`
        // keeps an Inline spec's backing file alive until the read completes.
        let (schema, batches, _tmp) = self.produce_materialize(&spec).await?;

        let entry = cache
            .materialize(name, schema, &batches, spec)
            .map_err(|e| EngineError::Internal(format!("materialize write: {e}")))?;

        Ok(MaterializeOutcome {
            name: TableName::new(pawrly_core::MATERIALIZED_SCHEMA, name),
            file_path: entry.file_path,
            row_count: entry.row_count,
            size_bytes: entry.size_bytes,
        })
    }

    async fn drop_materialized(
        &self,
        name: &str,
        namespace: Option<&str>,
    ) -> Result<bool, EngineError> {
        match self.inner.namespaces.for_read(namespace)? {
            Some(cache) => cache.drop_materialized(name),
            None => Ok(false),
        }
    }

    async fn drop_namespace(&self, namespace: &str) -> Result<bool, EngineError> {
        self.inner.namespaces.remove(namespace)
    }

    async fn add_source(&self, def: SourceDef) -> Result<SourceInfo, EngineError> {
        // Runtime-added sources get the same validation a config file gets
        // (plus the no-stdio rule); config-file sources were validated at load.
        let errors = pawrly_config::validate_engine_source(&def);
        if !errors.is_empty() {
            let msgs: Vec<String> = errors.0.iter().map(ToString::to_string).collect();
            return Err(EngineError::Internal(format!(
                "source `{}` failed validation: {}",
                def.name,
                msgs.join("; ")
            )));
        }
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

        // Reuse the engine's value store so a static secret set since the last
        // load resolves now (stored-wins), matching the build-time load path.
        let cfg = pawrly_config::load_auto_with_vars(&path, Some(self.inner.tokens.as_ref()))
            .map_err(|e| EngineError::Internal(e.to_string()))?;
        // Resolve declared functions before the config is consumed into sources.
        let new_function_defs = cfg.engine_functions();
        // Rebuild the dynamic-variable store + binding maps from the new config
        // (still intact here) so re-registered sources see current specs.
        *self.inner.variables.write() = Arc::new(
            pawrly_secrets::RuntimeVariableStore::with_tokens(
                cfg.dynamic_specs(),
                self.inner.tokens.clone(),
            )
            .with_cache_dir(self.inner.discovery_cache_dir.clone()),
        );
        *self.inner.dynamic_bindings.write() = cfg.dynamic_bindings_by_source();
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

        // Re-register all functions from the new config (builtins + declared).
        // Simplest correct reload: drop every current function + UDTF, then
        // rebuild — DataFusion overwrites a UDTF on re-registration anyway.
        let stale = self.inner.functions.write().drain_mangled();
        for m in stale {
            self.inner.ctx.deregister_udtf(&m);
        }
        register_functions(&self.inner, new_function_defs).await?;

        Ok(report)
    }

    async fn list_functions(&self) -> Result<Vec<pawrly_core::FunctionInfo>, EngineError> {
        Ok(self.inner.functions.read().infos())
    }

    async fn describe_function(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<pawrly_core::FunctionDescription, EngineError> {
        self.inner
            .functions
            .read()
            .describe(namespace, name)
            .ok_or_else(|| EngineError::UnknownFunction(format!("{namespace}.{name}")))
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

    async fn list_metrics(&self) -> Result<Vec<pawrly_core::semantic::Metric>, EngineError> {
        Ok(self.inner.semantic.list_metrics())
    }

    async fn describe_metric(
        &self,
        name: &str,
    ) -> Result<pawrly_core::semantic::Metric, EngineError> {
        self.inner
            .semantic
            .describe_metric(name)
            .ok_or_else(|| pawrly_semantic::SemanticError::UnknownMetric(name.to_string()).into())
    }

    #[tracing::instrument(name = "pawrly.engine.semantic_query", skip_all, fields(pawrly.engine = "local"))]
    async fn semantic_query(&self, q: SemanticQuery) -> Result<QueryHandle, EngineError> {
        let query_id = QueryId::new(uuid::Uuid::new_v4().to_string());
        let cancel_flag: crate::stream::CancelFlag = Arc::new(AtomicBool::new(false));
        self.inner
            .cancellations
            .lock()
            .insert(query_id.clone(), cancel_flag.clone());
        let completion = Arc::new(std::sync::OnceLock::new());
        let mut guard = crate::stream::QueryGuard::start(self.inner.active_queries.clone())
            .with_cancel(query_id.clone(), self.inner.cancellations.clone())
            .with_completion(completion.clone());
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
        Ok(QueryHandle::new(
            query_id,
            crate::stream::adapt_instrumented(stream, guard, Some(cancel_flag)),
            completion,
        ))
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
