//! `LocalEngine` — in-process implementation of `EngineService`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider};
use datafusion::execution::context::SessionContext;
use parking_lot::{Mutex, RwLock};
use pawrly_core::semantic::{SemanticModelDescription, SemanticModelInfo, SemanticQuery};
use pawrly_core::{
    CacheEntryInfo, CachePolicy, CatalogSnapshot, ColumnSpec, EngineError, EngineService,
    HealthReport, QueryId, QueryRequest, QueryStream, RefreshCatalogOutcome, RefreshOutcome,
    ReloadReport, SchemaSummary, SourceDef, SourceInfo, SourceStatus, SourceTestReport,
    TableDescription, TableFilter, TableInfo, TableName, TableSummary, VacuumReport,
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

struct LocalEngineInner {
    ctx: SessionContext,
    catalog: Arc<MemoryCatalogProvider>,
    sources: RwLock<HashMap<String, RegisteredSource>>,
    workspace_dir: PathBuf,
    cache: Arc<CacheManager>,
    /// Compiled semantic-layer models. Empty when no `semantic:` block exists.
    semantic: Arc<SemanticCatalog>,
    /// Background cache refreshers keyed by source name (one entry per
    /// `refresh`/`cron` table). Aborted on shutdown, source removal, and before
    /// a source is re-registered so re-registration never leaks tasks.
    refreshers: Mutex<HashMap<String, Vec<JoinHandle<()>>>>,
    /// Path the config was loaded from, when known. `reload_config` re-reads it.
    config_path: Option<PathBuf>,
    #[allow(
        dead_code,
        reason = "wired in M3; consumed by DuckDB-backed sources in M7"
    )]
    duckdb: Arc<DuckDbPool>,
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
        let ctx = SessionContext::new_with_config(session_config);
        let catalog: Arc<MemoryCatalogProvider> = Arc::new(MemoryCatalogProvider::new());
        // Register a `default` schema so `SELECT * FROM unqualified_table` resolves.
        let default_schema: Arc<dyn datafusion::catalog::SchemaProvider> =
            Arc::new(datafusion::catalog::MemorySchemaProvider::new());
        let _ = catalog
            .register_schema("default", default_schema)
            .map_err(|e| EngineError::Internal(format!("register default schema: {e}")))?;
        ctx.register_catalog(PAWRLY_CATALOG, catalog.clone());

        // The cache root comes from `defaults.cache.storage` (default
        // `~/.pawrly/cache`), NOT the workspace dir, so cached data lives under
        // `$HOME` regardless of where the CLI is invoked from. `~` / `~/` is
        // expanded against `$HOME`. A per-workspace namespace segment is then
        // appended so different workspaces sharing the same storage root never
        // collide on identical `schema.table` keys.
        let storage = expand_tilde(&cfg.config.defaults.cache.storage);
        let namespace = cache_namespace(
            cfg.config.defaults.cache.namespace.as_deref(),
            &cfg.workspace_dir,
        );
        let cache_root = storage.join(namespace);
        let cache = Arc::new(
            CacheManager::new(cache_root)
                .map_err(|e| EngineError::Internal(format!("cache init: {e}")))?,
        );

        let duckdb = Arc::new(DuckDbPool::new(cfg.resolved_pool_size())?);

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
        });

        // Move config into engine-side SourceDefs.
        let engine_sources = cfg.config.into_engine_sources();
        for def in engine_sources {
            register_source(&inner, def).await?;
        }
        Ok(Self { inner })
    }

    /// Convenience: load a YAML config from disk and build an engine in one step.
    /// The secret-resolution chain is built from the config's `secrets:` block
    /// (defaulting to the `auto` chain: env, keyring, then a `.env` file).
    pub async fn from_config_file(path: &std::path::Path) -> Result<Self, EngineError> {
        let cfg = pawrly_config::load_auto(path)
            .map_err(|e| EngineError::Internal(e.to_string()))?;
        // `workspace_dir` only anchors relative *source* paths to the config
        // file's directory. The `.pawrly/` data dir is resolved separately from
        // `defaults.cache.storage` (default `~/.pawrly`), not from here.
        let workspace_dir = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        Self::build(
            LocalEngineConfig {
                config: cfg,
                workspace_dir,
                duckdb_pool_size: None,
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
        };
        Self::new(LocalEngineConfig {
            config: cfg,
            workspace_dir,
            duckdb_pool_size: None,
        })
        .await
    }
}

/// Resolve the per-workspace cache namespace (a single path segment under the
/// shared storage root).
///
/// With an explicit `namespace` set in config, it is sanitized and used as-is
/// (so users can pin a stable id or deliberately share a cache). Otherwise a
/// stable id `<dirname>-<hash>` is derived from the canonicalized workspace
/// path, so distinct workspaces never collide on identical `schema.table`
/// names while the same workspace always maps to the same directory.
fn cache_namespace(explicit: Option<&str>, workspace_dir: &std::path::Path) -> String {
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
    let canonical = std::fs::canonicalize(workspace_dir)
        .unwrap_or_else(|_| workspace_dir.to_path_buf());
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
/// returned unchanged. Used to resolve the cache storage root so the default
/// `~/.pawrly/cache` lands under `$HOME`, not the workspace.
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
        let ns = cache_namespace(Some("My Cache/v2"), Path::new("/whatever"));
        assert_eq!(ns, "My-Cache-v2");
    }

    #[test]
    fn blank_explicit_namespace_falls_back_to_derived() {
        // An all-illegal-to-empty explicit value must not yield an empty segment.
        let ns = cache_namespace(Some("   "), Path::new("/tmp"));
        assert!(ns.contains('-') && !ns.starts_with('-'));
    }

    #[test]
    fn derived_namespace_is_stable_and_distinct() {
        // Same path → same id; different paths → different ids.
        let a1 = cache_namespace(None, Path::new("/tmp/ws-a-does-not-exist"));
        let a2 = cache_namespace(None, Path::new("/tmp/ws-a-does-not-exist"));
        let b = cache_namespace(None, Path::new("/tmp/ws-b-does-not-exist"));
        assert_eq!(a1, a2, "same workspace path must map to the same namespace");
        assert_ne!(a1, b, "distinct workspaces must not collide");
        assert!(a1.starts_with("ws-a-does-not-exist-"));
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
    async fn query(&self, req: QueryRequest) -> Result<QueryStream, EngineError> {
        let inner = self.inner.clone();
        let sql = req.sql.clone();
        // Substitute simple `${param:KEY}` occurrences.
        let sql = substitute_params(&sql, &req.params);

        let df = inner
            .ctx
            .sql(&sql)
            .await
            .map_err(|e| EngineError::InvalidSql(e.to_string()))?;
        let stream = df
            .execute_stream()
            .await
            .map_err(|e| EngineError::Internal(format!("datafusion: {e}")))?;
        Ok(crate::stream::adapt(stream))
    }

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

        let kind = self
            .inner
            .sources
            .read()
            .get(&name.schema)
            .map(|s| s.info.kind);
        let kind = kind.ok_or_else(|| EngineError::UnknownTable(name.to_string()))?;

        Ok(TableDescription {
            table: TableInfo {
                name: name.clone(),
                kind,
                description: None,
                row_count_estimate: None,
                cached: false,
                required_filters: Vec::new(),
            },
            columns,
            pushable_filter_columns: Vec::new(),
            examples: Vec::new(),
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
        self.inner.cache.refresh(name, &self.inner.ctx).await
    }

    async fn invalidate_cache(&self, name: &TableName) -> Result<bool, EngineError> {
        self.inner.cache.invalidate(name)
    }

    async fn vacuum_cache(&self) -> Result<VacuumReport, EngineError> {
        self.inner.cache.vacuum()
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

        let cfg = pawrly_config::load_auto(&path)
            .map_err(|e| EngineError::Internal(e.to_string()))?;
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

    async fn semantic_query(&self, q: SemanticQuery) -> Result<QueryStream, EngineError> {
        // Compile to SQL and execute through the same DataFusion path as `query`.
        let sql = self.inner.semantic.compile_sql(&q)?;
        let df = self
            .inner
            .ctx
            .sql(&sql)
            .await
            .map_err(|e| EngineError::InvalidSql(e.to_string()))?;
        let stream = df
            .execute_stream()
            .await
            .map_err(|e| EngineError::Internal(format!("datafusion: {e}")))?;
        Ok(crate::stream::adapt(stream))
    }

    async fn health(&self) -> Result<HealthReport, EngineError> {
        let sources = self.inner.sources.read();
        Ok(HealthReport {
            ok: true,
            version: env!("CARGO_PKG_VERSION").into(),
            active_queries: 0,
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

fn substitute_params(sql: &str, params: &HashMap<String, String>) -> String {
    let mut out = sql.to_string();
    for (k, v) in params {
        let needle = format!("${{param:{k}}}");
        out = out.replace(&needle, v);
    }
    out
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
