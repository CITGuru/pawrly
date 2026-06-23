//! Dependent (bind) joins for required-param HTTP tables.
//!
//! A get-by-id table can't be scanned without its id. When the id is a join key
//! (`top_stories t JOIN live_item i ON i.id = t.id`), `scan_bound` emits a
//! [`DeferredHttpScanExec`] and [`DependentJoinRule`] replaces the inner hash
//! join with a [`DependentJoinExec`]: it runs the driver once, takes the distinct
//! keys from that single snapshot (so non-deterministic ranked lists stay
//! consistent), fetches the probe via the `IN (...)` fan-out, and replays the
//! join over both materialised sides.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use arrow_array::RecordBatch;
use datafusion::common::{Column as CommonColumn, DataFusionError, JoinType, ScalarValue};
use datafusion::config::ConfigOptions;
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::execution::TaskContext;
use datafusion::logical_expr::Expr;
use datafusion::logical_expr::expr::InList;
use datafusion::physical_expr::expressions::Column;
use datafusion::physical_expr::{EquivalenceProperties, Partitioning};
use datafusion::physical_optimizer::PhysicalOptimizerRule;
use datafusion::physical_plan::execution_plan::{Boundedness, EmissionType};
use datafusion::physical_plan::joins::HashJoinExec;
use datafusion::physical_plan::limit::GlobalLimitExec;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, ExecutionPlan, PlanProperties, SendableRecordBatchStream, collect,
};
use futures::StreamExt as _;

use crate::deferred::DeferredHttpScanExec;

/// Rewrites an inner hash join over a [`DeferredHttpScanExec`] into a
/// [`DependentJoinExec`]. Registered last, so the captured driver subtree is
/// already optimised.
#[derive(Debug, Default)]
pub struct DependentJoinRule;

impl DependentJoinRule {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl PhysicalOptimizerRule for DependentJoinRule {
    fn optimize(
        &self,
        plan: Arc<dyn ExecutionPlan>,
        _config: &ConfigOptions,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        rewrite(plan, None)
    }

    fn name(&self) -> &str {
        "pawrly_dependent_join"
    }

    fn schema_check(&self) -> bool {
        true
    }
}

/// Rewrite the plan, threading the enclosing `LIMIT` down to a matched join so it
/// can cap its key fan-out.
fn rewrite(
    plan: Arc<dyn ExecutionPlan>,
    limit: Option<usize>,
) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
    if let Some(join) = plan.as_any().downcast_ref::<HashJoinExec>()
        && let Some(dep) = try_build_dependent_join(&plan, join, limit)
    {
        return Ok(dep);
    }

    let child_limit = limit_for_children(&plan, limit);
    let mut new_children = Vec::with_capacity(plan.children().len());
    let mut changed = false;
    for child in plan.children() {
        let rewritten = rewrite(child.clone(), child_limit)?;
        changed |= !Arc::ptr_eq(&rewritten, child);
        new_children.push(rewritten);
    }
    if changed {
        plan.with_new_children(new_children)
    } else {
        Ok(plan)
    }
}

/// The row cap to pass to a node's children: a `fetch`-bearing prefix operator (a
/// limit, or one fused into coalesce) bounds the child; a sort's `fetch` (TopK)
/// applies *after* reordering, so it — and everything else — clears the cap.
fn limit_for_children(plan: &Arc<dyn ExecutionPlan>, current: Option<usize>) -> Option<usize> {
    let any = plan.as_any();
    if let Some(gl) = any.downcast_ref::<GlobalLimitExec>() {
        return gl.fetch().map(|f| f.saturating_add(gl.skip()));
    }
    if is_prefix_preserving(any) {
        return plan.fetch().or(current);
    }
    None
}

/// Operators that emit a prefix of their input rows without reordering, so a
/// `LIMIT`/`fetch` above them bounds the child. (`CoalesceBatchesExec` is matched
/// defensively in case the planner still emits it.)
#[allow(
    deprecated,
    reason = "match the planner's nodes even if since-deprecated"
)]
fn is_prefix_preserving(any: &dyn Any) -> bool {
    use datafusion::physical_plan::coalesce_batches::CoalesceBatchesExec;
    use datafusion::physical_plan::coalesce_partitions::CoalescePartitionsExec;
    use datafusion::physical_plan::limit::LocalLimitExec;
    use datafusion::physical_plan::projection::ProjectionExec;
    use datafusion::physical_plan::repartition::RepartitionExec;
    any.is::<LocalLimitExec>()
        || any.is::<CoalesceBatchesExec>()
        || any.is::<CoalescePartitionsExec>()
        || any.is::<RepartitionExec>()
        || any.is::<ProjectionExec>()
}

/// Find a [`DeferredHttpScanExec`] under column-preserving wrappers (coalesce /
/// repartition — not projection, which would remap the key's column index).
fn find_deferred(plan: &Arc<dyn ExecutionPlan>) -> Option<Arc<dyn ExecutionPlan>> {
    if plan.as_any().is::<DeferredHttpScanExec>() {
        return Some(plan.clone());
    }
    if is_column_preserving(plan.as_any()) {
        let children = plan.children();
        if children.len() == 1 {
            return find_deferred(children[0]);
        }
    }
    None
}

/// Wrappers that preserve column order, so a key's index is stable across them.
#[allow(
    deprecated,
    reason = "match the planner's nodes even if since-deprecated"
)]
fn is_column_preserving(any: &dyn Any) -> bool {
    use datafusion::physical_plan::coalesce_batches::CoalesceBatchesExec;
    use datafusion::physical_plan::coalesce_partitions::CoalescePartitionsExec;
    use datafusion::physical_plan::repartition::RepartitionExec;
    any.is::<CoalesceBatchesExec>()
        || any.is::<CoalescePartitionsExec>()
        || any.is::<RepartitionExec>()
}

/// Match the dependent-join pattern on `join`, or return `None` to leave the plan
/// untouched (a never-bound deferred scan then errors at execution).
fn try_build_dependent_join(
    plan: &Arc<dyn ExecutionPlan>,
    join: &HashJoinExec,
    limit: Option<usize>,
) -> Option<Arc<dyn ExecutionPlan>> {
    if join.join_type() != &JoinType::Inner {
        return None;
    }
    let on = join.on();
    if on.len() != 1 {
        return None;
    }

    let (left_key, right_key) = &on[0];
    let left = join.left();
    let right = join.right();

    let (probe_is_right, deferred, probe_key, driver_key, probe_child) =
        match (find_deferred(left), find_deferred(right)) {
            (Some(d), None) => (false, d, left_key, right_key, left),
            (None, Some(d)) => (true, d, right_key, left_key, right),
            _ => return None,
        };

    let deferred_node = deferred.as_any().downcast_ref::<DeferredHttpScanExec>()?;

    // Probe key: a plain column naming one of the deferred scan's required params.
    let probe_col = probe_key.as_any().downcast_ref::<Column>()?;
    let probe_schema = probe_child.schema();
    if probe_col.index() >= probe_schema.fields().len() {
        return None;
    }
    let probe_name = probe_schema.field(probe_col.index()).name().clone();
    if !deferred_node.key_columns().iter().any(|k| k == &probe_name) {
        return None;
    }

    let driver_col = driver_key.as_any().downcast_ref::<Column>()?;
    let driver_key_idx = driver_col.index();

    Some(Arc::new(DependentJoinExec::new(
        plan.clone(),
        probe_is_right,
        deferred,
        driver_key_idx,
        probe_name,
        limit,
    )))
}

/// Bind-join operator: runs the driver once, fetches the probe for its keys, and
/// replays the original hash join over both materialised inputs.
#[derive(Debug)]
pub struct DependentJoinExec {
    join: Arc<dyn ExecutionPlan>,
    /// Whether the deferred probe is the join's right child.
    probe_is_right: bool,
    deferred: Arc<dyn ExecutionPlan>,
    /// Join-key column index in the driver's schema.
    driver_key_idx: usize,
    probe_key_col: String,
    /// Max distinct keys to fetch (`None` = uncapped).
    cap: Option<usize>,
    properties: Arc<PlanProperties>,
}

impl DependentJoinExec {
    fn new(
        join: Arc<dyn ExecutionPlan>,
        probe_is_right: bool,
        deferred: Arc<dyn ExecutionPlan>,
        driver_key_idx: usize,
        probe_key_col: String,
        cap: Option<usize>,
    ) -> Self {
        let properties = Arc::new(PlanProperties::new(
            EquivalenceProperties::new(join.schema()),
            Partitioning::UnknownPartitioning(1),
            EmissionType::Incremental,
            Boundedness::Bounded,
        ));
        Self {
            join,
            probe_is_right,
            deferred,
            driver_key_idx,
            probe_key_col,
            cap,
            properties,
        }
    }
}

impl DisplayAs for DependentJoinExec {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "DependentJoinExec: key={}, cap={:?}",
            self.probe_key_col, self.cap
        )
    }
}

impl ExecutionPlan for DependentJoinExec {
    fn name(&self) -> &str {
        "DependentJoinExec"
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
        partition: usize,
        context: Arc<TaskContext>,
    ) -> datafusion::common::Result<SendableRecordBatchStream> {
        if partition != 0 {
            return Err(DataFusionError::Internal(format!(
                "DependentJoinExec only has one partition, got {partition}"
            )));
        }

        let join = self.join.clone();
        let probe_is_right = self.probe_is_right;
        let deferred = self.deferred.clone();
        let driver_key_idx = self.driver_key_idx;
        let probe_key_col = self.probe_key_col.clone();
        let cap = self.cap;
        let schema = self.schema();

        let fut = async move {
            // Driver side, collected once — the snapshot the keys come from.
            let children = join.children();
            let driver_plan = if probe_is_right {
                children[0].clone()
            } else {
                children[1].clone()
            };
            let driver_schema = driver_plan.schema();
            let driver_batches = collect(driver_plan, context.clone()).await?;

            let keys = distinct_keys(&driver_batches, driver_key_idx, cap)?;
            if keys.is_empty() {
                return Ok(Vec::new());
            }

            let deferred_node = deferred
                .as_any()
                .downcast_ref::<DeferredHttpScanExec>()
                .ok_or_else(|| {
                    DataFusionError::Internal("dependent join lost its deferred scan".into())
                })?;
            let in_expr = build_in_list(&probe_key_col, &keys);
            let probe_exec = deferred_node.scan_with_filters(vec![in_expr]).await?;

            let driver_mem =
                MemorySourceConfig::try_new_exec(&[driver_batches], driver_schema, None)?;
            let new_children: Vec<Arc<dyn ExecutionPlan>> = if probe_is_right {
                vec![driver_mem, probe_exec]
            } else {
                vec![probe_exec, driver_mem]
            };
            let rebuilt = join.with_new_children(new_children)?;
            collect(rebuilt, context).await
        };

        let stream = futures::stream::once(fut).flat_map(
            |res: datafusion::common::Result<Vec<RecordBatch>>| match res {
                Ok(batches) => {
                    futures::stream::iter(batches.into_iter().map(Ok).collect::<Vec<_>>())
                }
                Err(e) => futures::stream::iter(vec![Err(e)]),
            },
        );
        Ok(Box::pin(RecordBatchStreamAdapter::new(schema, stream)))
    }
}

/// Distinct, non-null values of column `idx`, first-seen order, up to `cap`.
fn distinct_keys(
    batches: &[RecordBatch],
    idx: usize,
    cap: Option<usize>,
) -> datafusion::common::Result<Vec<ScalarValue>> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<ScalarValue> = Vec::new();
    for b in batches {
        if idx >= b.num_columns() {
            continue;
        }
        let col = b.column(idx);
        for row in 0..b.num_rows() {
            if cap.is_some_and(|c| out.len() >= c) {
                return Ok(out);
            }
            let value = ScalarValue::try_from_array(col, row)?;
            if value.is_null() {
                continue;
            }
            if seen.insert(value.clone()) {
                out.push(value);
            }
        }
    }
    Ok(out)
}

/// Build `col IN (k0, k1, …)` for the typed scan's fan-out.
fn build_in_list(col: &str, keys: &[ScalarValue]) -> Expr {
    let col_expr = Expr::Column(CommonColumn::new_unqualified(col));
    let list = keys
        .iter()
        .map(|k| Expr::Literal(k.clone(), None))
        .collect();
    Expr::InList(InList::new(Box::new(col_expr), list, false))
}
