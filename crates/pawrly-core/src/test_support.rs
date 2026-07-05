//! In-memory `EngineService` implementation for testing.
//!
//! Enable with `--features test-support`.

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use async_trait::async_trait;
use chrono::Utc;
use parking_lot::Mutex;

use crate::cache::{CacheEntryInfo, RefreshOutcome, VacuumReport};
use crate::error::EngineError;
use crate::model::SourceKind;
use crate::schema::{
    CatalogSnapshot, ColumnSpec, SchemaSummary, TableDescription, TableFilter, TableInfo,
    TableName, TableSummary,
};
use crate::semantic::{SemanticModelDescription, SemanticModelInfo, SemanticQuery};
use crate::service::{
    EngineService, MaterializeOutcome, MaterializeSpec, QueryHandle, QueryRequest,
};
use crate::source::{
    HealthReport, RefreshCatalogOutcome, ReloadReport, SourceDef, SourceInfo, SourceStatus,
    SourceTestReport,
};

/// Programmable canned-response engine for tests.
///
/// Construct with [`MockEngine::new`], then push canned tables and
/// query responses with the builder methods. Every `query` call records
/// the SQL string so tests can assert on what was issued.
#[derive(Default)]
pub struct MockEngine {
    inner: Arc<Mutex<MockState>>,
}

#[derive(Default)]
struct MockState {
    sources: HashMap<String, SourceInfo>,
    tables: HashMap<TableName, (TableInfo, TableDescription)>,
    canned_queries: HashMap<String, Vec<RecordBatch>>,
    queries_seen: Vec<String>,
    cache_entries: Vec<CacheEntryInfo>,
    functions: Vec<crate::function::FunctionDef>,
}

impl MockEngine {
    /// Create an empty `MockEngine`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fake table-valued function. Surfaces through `list_functions`
    /// and `describe_function`.
    pub fn add_function(&self, def: crate::function::FunctionDef) -> &Self {
        self.inner.lock().functions.push(def);
        self
    }

    /// Register a fake source. Adds it to `list_sources`.
    pub fn add_source(&self, name: impl Into<String>, kind: SourceKind) -> &Self {
        let name = name.into();
        self.inner.lock().sources.insert(
            name.clone(),
            SourceInfo {
                name,
                kind,
                status: SourceStatus::Ok,
                status_detail: None,
                sub_kind: None,
                table_count: 0,
                registered_at: Utc::now(),
            },
        );
        self
    }

    /// Register a fake table. Adds it to `list_tables` and `describe_table`.
    pub fn add_table(&self, name: TableName, kind: SourceKind, columns: Vec<ColumnSpec>) -> &Self {
        let info = TableInfo {
            name: name.clone(),
            kind,
            description: None,
            row_count_estimate: None,
            cached: false,
            required_filters: Vec::new(),
        };
        let desc = TableDescription {
            table: info.clone(),
            columns,
            pushable_filter_columns: Vec::new(),
            examples: Vec::new(),
            wiki: None,
        };
        self.inner.lock().tables.insert(name, (info, desc));
        self
    }

    /// Register a fake table carrying a description (for catalog-search tests).
    pub fn add_table_with_description(
        &self,
        name: TableName,
        kind: SourceKind,
        description: impl Into<String>,
    ) -> &Self {
        let info = TableInfo {
            name: name.clone(),
            kind,
            description: Some(description.into()),
            row_count_estimate: None,
            cached: false,
            required_filters: Vec::new(),
        };
        let desc = TableDescription {
            table: info.clone(),
            columns: Vec::new(),
            pushable_filter_columns: Vec::new(),
            examples: Vec::new(),
            wiki: None,
        };
        self.inner.lock().tables.insert(name, (info, desc));
        self
    }

    /// Pre-stage a canned response for queries whose SQL contains `needle`.
    /// Lookups use `contains` so tests can match a fragment without having
    /// to reproduce the full SQL string.
    pub fn canned(&self, needle: impl Into<String>, batches: Vec<RecordBatch>) -> &Self {
        self.inner
            .lock()
            .canned_queries
            .insert(needle.into(), batches);
        self
    }

    /// Snapshot of every SQL string seen by `query` so far.
    #[must_use]
    pub fn queries_seen(&self) -> Vec<String> {
        self.inner.lock().queries_seen.clone()
    }

    /// Build a one-row two-column `RecordBatch` for canned-query convenience.
    ///
    /// The shape `(id Int64, label Utf8)` is well-formed by construction;
    /// `try_new` cannot fail here so we surface the error as `expect`.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "schema and arrays are constructed together; mismatch is unreachable"
    )]
    pub fn one_row(id: i64, label: &str) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("label", DataType::Utf8, true),
        ]));
        let id_arr = Arc::new(Int64Array::from(vec![id]));
        let label_arr = Arc::new(StringArray::from(vec![label.to_string()]));
        RecordBatch::try_new(schema, vec![id_arr, label_arr])
            .expect("MockEngine::one_row: schema/array mismatch is unreachable")
    }
}

#[async_trait]
impl EngineService for MockEngine {
    async fn query(&self, req: QueryRequest) -> Result<QueryHandle, EngineError> {
        let mut state = self.inner.lock();
        state.queries_seen.push(req.sql.clone());
        let batches = state
            .canned_queries
            .iter()
            .find_map(|(needle, batches)| {
                if req.sql.contains(needle) {
                    Some(batches.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        drop(state);

        let stream = async_stream::stream! {
            for batch in batches {
                yield Ok(batch);
            }
        };
        Ok(QueryHandle::detached(Box::pin(stream)))
    }

    async fn explain(&self, sql: &str, _analyze: bool) -> Result<String, EngineError> {
        Ok(format!("Plan(mock): {sql}"))
    }

    async fn cancel(&self, _query_id: &crate::service::QueryId) -> Result<bool, EngineError> {
        Ok(false)
    }

    async fn list_sources(&self) -> Result<Vec<SourceInfo>, EngineError> {
        Ok(self.inner.lock().sources.values().cloned().collect())
    }

    async fn list_tables(
        &self,
        filter: Option<TableFilter>,
    ) -> Result<Vec<TableInfo>, EngineError> {
        let state = self.inner.lock();
        let mut out: Vec<_> = state
            .tables
            .values()
            .map(|(info, _)| info.clone())
            .collect();
        if let Some(f) = filter {
            if let Some(src) = f.source.as_deref() {
                out.retain(|t| t.name.schema == src);
            }
        }
        Ok(out)
    }

    async fn describe_table(&self, name: &TableName) -> Result<TableDescription, EngineError> {
        self.inner
            .lock()
            .tables
            .get(name)
            .map(|(_, d)| d.clone())
            .ok_or_else(|| EngineError::UnknownTable(name.to_string()))
    }

    async fn schema_snapshot(
        &self,
        sources: Option<Vec<String>>,
        _compact: bool,
    ) -> Result<CatalogSnapshot, EngineError> {
        let state = self.inner.lock();
        let mut by_schema: HashMap<String, (SourceKind, Vec<TableSummary>)> = HashMap::new();
        for (name, (info, _)) in &state.tables {
            if let Some(filter) = &sources
                && !filter.contains(&name.schema)
            {
                continue;
            }
            by_schema
                .entry(name.schema.clone())
                .or_insert((info.kind, Vec::new()))
                .1
                .push(TableSummary {
                    name: name.table.clone(),
                    columns: String::new(),
                    required_filters: info.required_filters.clone(),
                });
        }
        let schemas = by_schema
            .into_iter()
            .map(|(name, (kind, tables))| SchemaSummary { name, kind, tables })
            .collect();
        Ok(CatalogSnapshot { schemas })
    }

    async fn refresh_catalog(
        &self,
        _source: Option<&str>,
    ) -> Result<RefreshCatalogOutcome, EngineError> {
        Ok(RefreshCatalogOutcome::default())
    }

    async fn cache_entries(&self) -> Result<Vec<CacheEntryInfo>, EngineError> {
        Ok(self.inner.lock().cache_entries.clone())
    }

    async fn refresh_table(&self, name: &TableName) -> Result<RefreshOutcome, EngineError> {
        Ok(RefreshOutcome {
            table: name.clone(),
            rows_written: 0,
            size_bytes: 0,
            elapsed: std::time::Duration::ZERO,
            expires_at: None,
        })
    }

    async fn invalidate_cache(&self, _name: &TableName) -> Result<bool, EngineError> {
        Ok(false)
    }

    async fn vacuum_cache(&self) -> Result<VacuumReport, EngineError> {
        Ok(VacuumReport::default())
    }

    async fn materialize(
        &self,
        name: &str,
        _spec: MaterializeSpec,
    ) -> Result<MaterializeOutcome, EngineError> {
        Ok(MaterializeOutcome {
            name: TableName::new(crate::MATERIALIZED_SCHEMA, name),
            file_path: std::path::PathBuf::new(),
            row_count: 0,
            size_bytes: 0,
        })
    }

    async fn drop_materialized(&self, _name: &str) -> Result<bool, EngineError> {
        Ok(false)
    }

    async fn add_source(&self, def: SourceDef) -> Result<SourceInfo, EngineError> {
        let info = SourceInfo {
            name: def.name.clone(),
            kind: def.kind,
            status: SourceStatus::Ok,
            status_detail: None,
            sub_kind: None,
            table_count: def.tables.len() as u64,
            registered_at: Utc::now(),
        };
        self.inner.lock().sources.insert(def.name, info.clone());
        Ok(info)
    }

    async fn remove_source(&self, name: &str) -> Result<bool, EngineError> {
        Ok(self.inner.lock().sources.remove(name).is_some())
    }

    async fn test_source(&self, name: &str) -> Result<SourceTestReport, EngineError> {
        Ok(SourceTestReport {
            name: name.to_string(),
            ok: true,
            latency: std::time::Duration::from_millis(1),
            detail: Some("mock".into()),
        })
    }

    async fn reload_config(&self) -> Result<ReloadReport, EngineError> {
        Ok(ReloadReport::default())
    }

    async fn list_functions(&self) -> Result<Vec<crate::function::FunctionInfo>, EngineError> {
        Ok(self
            .inner
            .lock()
            .functions
            .iter()
            .map(|d| d.info())
            .collect())
    }

    async fn describe_function(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<crate::function::FunctionDescription, EngineError> {
        self.inner
            .lock()
            .functions
            .iter()
            .find(|d| d.namespace == namespace && d.name == name)
            .map(crate::function::FunctionDef::describe)
            .ok_or_else(|| EngineError::UnknownFunction(format!("{namespace}.{name}")))
    }

    async fn list_semantic_models(&self) -> Result<Vec<SemanticModelInfo>, EngineError> {
        Ok(Vec::new())
    }

    async fn describe_semantic_model(
        &self,
        name: &str,
    ) -> Result<SemanticModelDescription, EngineError> {
        Err(EngineError::SemanticPlan(format!(
            "unknown semantic model `{name}`"
        )))
    }

    async fn semantic_query(&self, _q: SemanticQuery) -> Result<QueryHandle, EngineError> {
        // MockEngine returns no rows for semantic queries by default.
        let batches: Vec<RecordBatch> = Vec::new();
        let stream = async_stream::stream! {
            for batch in batches {
                yield Ok(batch);
            }
        };
        Ok(QueryHandle::detached(Box::pin(stream)))
    }

    async fn health(&self) -> Result<HealthReport, EngineError> {
        let state = self.inner.lock();
        Ok(HealthReport {
            ok: true,
            version: env!("CARGO_PKG_VERSION").into(),
            active_queries: 0,
            sources_ok: state.sources.len() as u64,
            sources_unavailable: 0,
        })
    }

    async fn shutdown(&self) -> Result<(), EngineError> {
        Ok(())
    }
}
