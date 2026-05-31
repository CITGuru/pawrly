//! Semantic pre-aggregation (rollup) materialization
//!
//! At boot, [`register_rollups`] turns each declared `PreAggregation` into a
//! synthetic table `"semantic"."<model>__<preagg>"`. The table's provider runs
//! the pre-agg's aggregate SELECT (a [`RollupProvider`]) and is wrapped in a
//! [`CachedTableProvider`], so the result is materialized to Parquet and a query
//! reads the cached rollup rather than re-aggregating the base table. A pre-agg
//! with `refresh:` set gets a background refresher (the same `Spawner` the
//! source caches use); the rest are materialized lazily on first use and stay
//! until invalidated.
//!
//! Materialization is lazy: nothing is built at boot. The first semantic query a
//! rollup covers triggers a write-through (see `LocalEngine::semantic_query`),
//! and the background refresher — when configured — keeps it current.

use std::any::Any;
use std::sync::Arc;

use arrow_schema::SchemaRef;
use async_trait::async_trait;
use datafusion::catalog::{CatalogProvider, MemorySchemaProvider, SchemaProvider, Session};
use datafusion::datasource::{TableProvider, TableType};
use datafusion::execution::context::SessionContext;
use datafusion::logical_expr::Expr;
use datafusion::physical_plan::ExecutionPlan;
use pawrly_core::{CachePolicy, EngineError, TableName};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::cache::CachedTableProvider;
use crate::cache::refresher::Spawner;
use crate::local::LocalEngineInner;

/// Refresher bucket key under which rollup refreshers are tracked (they are not
/// owned by any one source).
pub(crate) const ROLLUP_REFRESHER_KEY: &str = "__rollups__";

/// A `TableProvider` whose rows are the result of a fixed materialization SQL
/// run against the engine context. Re-planned on every scan so it reflects the
/// current catalog; because it is wrapped in a [`CachedTableProvider`], `scan`
/// only runs on a cache miss or a refresh.
struct RollupProvider {
    ctx: SessionContext,
    sql: Arc<str>,
    schema: SchemaRef,
}

impl std::fmt::Debug for RollupProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RollupProvider")
            .field("sql", &self.sql)
            .finish()
    }
}

#[async_trait]
impl TableProvider for RollupProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        _projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        _limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        // Re-plan against the live context; the cache layer handles projection.
        let df = self.ctx.sql(&self.sql).await?;
        df.create_physical_plan().await
    }
}

/// Register every declared pre-aggregation as a cached rollup table under the
/// `semantic` schema, spawning a background refresher for those with `refresh:`.
/// Idempotent: re-registering replaces prior rollup tables and refreshers.
pub(crate) async fn register_rollups(inner: &Arc<LocalEngineInner>) -> Result<(), EngineError> {
    let specs = inner.semantic.rollups();
    if specs.is_empty() {
        return Ok(());
    }

    let schema_name = pawrly_semantic::rollup::ROLLUP_SCHEMA;
    let schema: Arc<dyn SchemaProvider> = match inner.catalog.schema(schema_name) {
        Some(existing) => existing,
        None => {
            let provider: Arc<dyn SchemaProvider> = Arc::new(MemorySchemaProvider::new());
            inner
                .catalog
                .register_schema(schema_name, provider.clone())
                .map_err(|e| EngineError::Internal(format!("register semantic schema: {e}")))?;
            provider
        }
    };

    // Replace any prior rollup refreshers so re-registration never leaks tasks.
    if let Some(handles) = inner.refreshers.lock().remove(ROLLUP_REFRESHER_KEY) {
        for h in handles {
            h.abort();
        }
    }

    let mut spawned: Vec<JoinHandle<()>> = Vec::new();
    for spec in specs {
        // A pre-agg that can't be compiled or planned (e.g. a `DATE_TRUNC` over a
        // column inferred as text) is skipped, never fatal: the query just falls
        // through to the base table, same as a missing rollup.
        let sql = match inner.semantic.compile_preagg_sql(&spec.model, &spec.preagg) {
            Ok(sql) => sql,
            Err(e) => {
                tracing::warn!(
                    model = %spec.model, preagg = %spec.preagg, error = %e,
                    "pre-aggregation could not be compiled; rollup disabled"
                );
                continue;
            }
        };

        // Plan once to learn the output schema (and validate the SQL early).
        let df = match inner.ctx.sql(&sql).await {
            Ok(df) => df,
            Err(e) => {
                tracing::warn!(
                    model = %spec.model, preagg = %spec.preagg, error = %e,
                    "pre-aggregation could not be planned; rollup disabled"
                );
                continue;
            }
        };
        let schema_ref: SchemaRef = Arc::new(df.schema().as_arrow().clone());

        let provider = Arc::new(RollupProvider {
            ctx: inner.ctx.clone(),
            sql: Arc::from(sql.as_str()),
            schema: schema_ref,
        });

        let table = pawrly_semantic::rollup::rollup_table_name(&spec.model, &spec.preagg);
        let key = TableName::new(schema_name.to_string(), table.clone());
        // `refresh:` → background refresh; otherwise materialize once and stay
        // fresh (a zero interval is never scheduled, only stored).
        let policy = match spec.refresh {
            Some(every) => CachePolicy::Refresh { every },
            None => CachePolicy::Refresh {
                every: std::time::Duration::ZERO,
            },
        };

        let wrapped =
            CachedTableProvider::wrap(provider, key.clone(), policy.clone(), inner.cache.clone());
        let _ = schema.deregister_table(&table);
        schema
            .register_table(table.clone(), wrapped)
            .map_err(|e| EngineError::Internal(format!("register rollup `{table}`: {e}")))?;

        if spec.refresh.is_some()
            && let Some(handle) = Spawner::spawn_for(
                &Handle::current(),
                key,
                policy,
                inner.cache.clone(),
                inner.ctx.clone(),
            )
        {
            spawned.push(handle);
        }
    }

    if !spawned.is_empty() {
        inner
            .refreshers
            .lock()
            .insert(ROLLUP_REFRESHER_KEY.to_string(), spawned);
    }
    Ok(())
}
