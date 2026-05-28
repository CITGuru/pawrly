//! `LocalEngine` — in-process implementation of `EngineService`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider};
use datafusion::execution::context::SessionContext;
use parking_lot::{Mutex, RwLock};
use pawrly_core::{
    CacheEntryInfo, CachePolicy, CatalogSnapshot, ColumnSpec, EngineError, EngineService,
    HealthReport, QueryId, QueryRequest, QueryStream, RefreshCatalogOutcome, RefreshOutcome,
    ReloadReport, SchemaSummary, SourceDef, SourceInfo, SourceStatus, SourceTestReport,
    TableDescription, TableFilter, TableInfo, TableName, TableSummary, VacuumReport,
};
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

        let cache_root = cfg.workspace_dir.join(".pawrly").join("cache");
        let cache = Arc::new(
            CacheManager::new(cache_root)
                .map_err(|e| EngineError::Internal(format!("cache init: {e}")))?,
        );

        let duckdb = Arc::new(DuckDbPool::new(cfg.resolved_pool_size())?);

        let inner = Arc::new(LocalEngineInner {
            ctx,
            catalog,
            sources: RwLock::new(HashMap::new()),
            workspace_dir: cfg.workspace_dir.clone(),
            cache,
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
    /// Uses the default secret chain (env + keyring with service=pawrly).
    pub async fn from_config_file(path: &std::path::Path) -> Result<Self, EngineError> {
        let secrets = pawrly_secrets::default_chain();
        let cfg = pawrly_config::load(path, &secrets)
            .map_err(|e| EngineError::Internal(e.to_string()))?;
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
        };
        Self::new(LocalEngineConfig {
            config: cfg,
            workspace_dir,
            duckdb_pool_size: None,
        })
        .await
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

        let secrets = pawrly_secrets::default_chain();
        let cfg = pawrly_config::load(&path, &secrets)
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
