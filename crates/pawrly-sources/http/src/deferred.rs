//! `DeferredHttpScanExec` — placeholder scan for a typed HTTP table whose
//! required param is unbound.
//!
//! When the id is only known at runtime (a join key), `scan_bound` returns this
//! instead of failing the plan; [`crate::dependent_join`] binds it inside a join.
//! Executed unbound — a bare `SELECT` with the required filter missing — it fails
//! with the same `PAWRLY_SAFETY_REQUIRED_FILTER` a normal scan would have.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use arrow_schema::Schema;
use datafusion::common::DataFusionError;
use datafusion::execution::TaskContext;
use datafusion::logical_expr::Expr;
use datafusion::physical_expr::{EquivalenceProperties, Partitioning};
use datafusion::physical_plan::execution_plan::{Boundedness, EmissionType};
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, ExecutionPlan, PlanProperties, SendableRecordBatchStream,
};

use crate::source::{HttpSource, HttpTableSpec};
use crate::typed::HttpTableProvider;

/// Holds everything needed to fetch once a join supplies the required key.
#[derive(Debug)]
pub struct DeferredHttpScanExec {
    source: Arc<HttpSource>,
    spec: Arc<HttpTableSpec>,
    max_pages: Option<u32>,
    max_rows: Option<u64>,
    projection: Option<Vec<usize>>,
    /// Filters already bound; re-applied with the runtime key filter.
    filters: Vec<Expr>,
    /// Unbound required params — the candidate join keys.
    key_columns: Vec<String>,
    properties: Arc<PlanProperties>,
}

impl DeferredHttpScanExec {
    pub fn new(
        provider: &HttpTableProvider,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        key_columns: Vec<String>,
    ) -> Self {
        let full_schema = provider.schema.clone();
        let projected_schema = match projection {
            Some(p) => Arc::new(Schema::new(
                p.iter()
                    .map(|i| full_schema.field(*i).clone())
                    .collect::<Vec<_>>(),
            )),
            None => full_schema,
        };
        let properties = Arc::new(PlanProperties::new(
            EquivalenceProperties::new(projected_schema.clone()),
            Partitioning::UnknownPartitioning(1),
            EmissionType::Incremental,
            Boundedness::Bounded,
        ));
        Self {
            source: provider.source.clone(),
            spec: provider.spec.clone(),
            max_pages: provider.max_pages,
            max_rows: provider.max_rows,
            projection: projection.cloned(),
            filters: filters.to_vec(),
            key_columns,
            properties,
        }
    }

    pub fn key_columns(&self) -> &[String] {
        &self.key_columns
    }

    /// Fetch the probe: the original filters plus `extra_filters` (the runtime
    /// `key IN (...)`). `allow_defer = false`, so a still-unbound required param
    /// errors rather than deferring again.
    pub async fn scan_with_filters(
        &self,
        extra_filters: Vec<Expr>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let provider = HttpTableProvider::with_safety(
            self.source.clone(),
            self.spec.clone(),
            self.max_pages,
            self.max_rows,
        );
        let mut filters = self.filters.clone();
        filters.extend(extra_filters);
        provider
            .scan_bound(self.projection.as_ref(), &filters, None, false)
            .await
    }
}

impl DisplayAs for DeferredHttpScanExec {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "DeferredHttpScanExec: table={}, keys=[{}]",
            self.spec.name,
            self.key_columns.join(", ")
        )
    }
}

impl ExecutionPlan for DeferredHttpScanExec {
    fn name(&self) -> &str {
        "DeferredHttpScanExec"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn properties(&self) -> &Arc<PlanProperties> {
        &self.properties
    }

    fn children(&self) -> Vec<&Arc<dyn ExecutionPlan>> {
        vec![]
    }

    fn with_new_children(
        self: Arc<Self>,
        _children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        Ok(self)
    }

    fn execute(
        &self,
        _partition: usize,
        _context: Arc<TaskContext>,
    ) -> datafusion::common::Result<SendableRecordBatchStream> {
        // Reached only if no join bound the key — the required filter is missing.
        Err(DataFusionError::Plan(format!(
            "table `{}` requires filter `{} = ...` (PAWRLY_SAFETY_REQUIRED_FILTER)",
            self.spec.name,
            self.key_columns.first().map(String::as_str).unwrap_or("?")
        )))
    }
}
