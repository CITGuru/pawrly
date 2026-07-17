//! The metric pass: expands dot-free metric members into leaf measures,
//! compiles the expanded query through the unchanged measure pipeline, and
//! projects each metric as arithmetic over the aggregated columns.
//!
//! A metric-level / per-operand / per-token filter turns its leaf into a
//! distinct **synthetic measure** on a clone of the owning model — the filter
//! lands in the leaf's `FILTER (WHERE …)` clause, and two differently-filtered
//! references to one measure produce two leaf columns. Leaves the caller did
//! not explicitly request never appear in the output projection.

use std::collections::HashMap;
use std::sync::Arc;

use pawrly_core::TableName;
use pawrly_core::semantic::{
    CumulativeWindow, Measure, Metric, MetricKind, OffsetOutput, SemanticModel, SemanticQuery,
    TimeGrain, WindowAgg, derived_tokens,
};

use crate::{SemanticCatalog, SemanticError, quote_ident};

/// Alias of the aggregated relation the metric projection selects from.
const AGG_ALIAS: &str = "__agg";
/// Alias of the dense time axis window metrics join against.
const AXIS_ALIAS: &str = "__axis";

pub(crate) fn compile_with_metrics(
    catalog: &SemanticCatalog,
    q: &SemanticQuery,
) -> Result<String, SemanticError> {
    // Cumulative/offset join the aggregate onto a dense time axis so data
    // gaps can't misalign the window; dimensions then read from the axis side.
    let spine_mode = q
        .measures
        .iter()
        .filter(|m| !m.contains('.'))
        .any(|m| has_window(catalog, m, &mut Vec::new()));
    let dims_alias = if spine_mode { AXIS_ALIAS } else { AGG_ALIAS };

    let time_dims: Vec<(String, TimeGrain)> = q
        .dimensions
        .iter()
        .filter_map(|d| {
            let parts: Vec<&str> = d.split('.').collect();
            match parts.as_slice() {
                [_, _, grain] => TimeGrain::parse(grain).map(|g| (d.clone(), g)),
                _ => None,
            }
        })
        .collect();

    let mut expansion = Expansion {
        catalog,
        q,
        time_dims,
        dims_alias,
        leaves: Vec::new(),
        leaf_index: HashMap::new(),
        synthetic: HashMap::new(),
    };

    let mut projections: Vec<String> = Vec::new();
    for d in &q.dimensions {
        let col = quote_ident(d);
        projections.push(format!("{dims_alias}.{col} AS {col}"));
    }
    for m in &q.measures {
        if m.contains('.') {
            expansion.leaf(m, &[])?;
            let col = quote_ident(m);
            projections.push(format!("{AGG_ALIAS}.{col} AS {col}"));
        } else {
            let expr = expansion.metric_expr(m, &[], &mut Vec::new())?;
            projections.push(format!("{expr} AS {}", quote_ident(m)));
        }
    }

    // Ordering and the caller's limit move to the outer SELECT — they may
    // reference metric columns that only exist there.
    let mut inner = q.clone();
    inner.measures = expansion.leaves.clone();
    inner.order_by = Vec::new();
    inner.limit = None;
    let inner_sql = expansion.augmented_catalog().compile_sql(&inner)?;

    let mut sql = if spine_mode {
        let axis = expansion.axis_sql()?;
        let join = q
            .dimensions
            .iter()
            .map(|d| {
                let col = quote_ident(d);
                format!("{AXIS_ALIAS}.{col} IS NOT DISTINCT FROM {AGG_ALIAS}.{col}")
            })
            .collect::<Vec<_>>()
            .join(" AND ");
        format!(
            "WITH {AGG_ALIAS} AS (\n{inner_sql}\n),\n{AXIS_ALIAS} AS (\n{axis}\n)\n\
             SELECT {}\nFROM {AXIS_ALIAS}\nLEFT JOIN {AGG_ALIAS} ON {join}",
            projections.join(", ")
        )
    } else {
        format!(
            "SELECT {}\nFROM (\n{inner_sql}\n) AS {AGG_ALIAS}",
            projections.join(", ")
        )
    };
    let orders = catalog.resolve_orders(q)?;
    if !orders.is_empty() {
        sql.push_str("\nORDER BY ");
        sql.push_str(&orders.join(", "));
    }
    // Root-model row caps were already applied by the inner compile; only the
    // caller's own limit belongs out here.
    if let Some(limit) = q.limit {
        sql.push_str(&format!("\nLIMIT {limit}"));
    }
    Ok(sql)
}

/// True when `name` (or any metric it transitively references) is a
/// cumulative/offset metric and therefore needs the time axis.
fn has_window(catalog: &SemanticCatalog, name: &str, visited: &mut Vec<String>) -> bool {
    let Some(metric) = catalog.metrics.get(name) else {
        return false;
    };
    if visited.iter().any(|v| v == name) {
        return false;
    }
    visited.push(name.to_string());
    match &metric.kind {
        MetricKind::Cumulative { .. } | MetricKind::Offset { .. } => true,
        MetricKind::Share { .. } => false,
        MetricKind::Ratio { .. } | MetricKind::Derived { .. } => metric
            .references()
            .unwrap_or_default()
            .iter()
            .any(|(member, _)| !member.contains('.') && has_window(catalog, member, visited)),
    }
}

fn window_fn(agg: WindowAgg) -> &'static str {
    match agg {
        WindowAgg::Sum => "SUM",
        WindowAgg::Avg => "AVG",
        WindowAgg::Min => "MIN",
        WindowAgg::Max => "MAX",
    }
}

fn grain_step(grain: TimeGrain) -> &'static str {
    match grain {
        TimeGrain::Hour => "1 hour",
        TimeGrain::Day => "1 day",
        TimeGrain::Week => "1 week",
        TimeGrain::Month => "1 month",
        TimeGrain::Quarter => "3 months",
        TimeGrain::Year => "1 year",
    }
}

/// Accumulates the leaf-measure set (and any synthetic filtered measures)
/// while rendering metric expressions.
struct Expansion<'c> {
    catalog: &'c SemanticCatalog,
    q: &'c SemanticQuery,
    /// The query's time-grained dimensions; window metrics need exactly one.
    time_dims: Vec<(String, TimeGrain)>,
    /// Where dimension columns live in the outer SELECT (`__axis` in spine
    /// mode, else `__agg`).
    dims_alias: &'static str,
    /// Inner-query members, insertion-ordered and deduped.
    leaves: Vec<String>,
    /// `(member, sorted filters)` → inner-query member carrying that leaf.
    leaf_index: HashMap<(String, Vec<String>), String>,
    /// Synthetic filtered measures to graft onto cloned models.
    synthetic: HashMap<String, Vec<Measure>>,
}

impl Expansion<'_> {
    /// Render `metric` as a scalar expression over the aggregated columns.
    /// `inherited` carries filters from enclosing metrics/operands; `visiting`
    /// is the metric-reference stack for cycle detection.
    fn metric_expr(
        &mut self,
        name: &str,
        inherited: &[String],
        visiting: &mut Vec<String>,
    ) -> Result<String, SemanticError> {
        let metric: Arc<Metric> = self
            .catalog
            .metrics
            .get(name)
            .cloned()
            .ok_or_else(|| SemanticError::UnknownMetric(name.to_string()))?;
        if visiting.iter().any(|v| v == name) {
            return Err(SemanticError::MetricCycle(name.to_string()));
        }
        visiting.push(name.to_string());

        let mut base: Vec<String> = inherited.to_vec();
        if let Some(f) = &metric.filter {
            base.push(f.clone());
        }

        let expr = match &metric.kind {
            MetricKind::Ratio {
                numerator,
                denominator,
            } => {
                let num = self.member_expr(
                    &numerator.member,
                    &and_filter(&base, numerator.filter.as_ref()),
                    visiting,
                )?;
                let den = self.member_expr(
                    &denominator.member,
                    &and_filter(&base, denominator.filter.as_ref()),
                    visiting,
                )?;
                // Integer division would silently truncate the ratio.
                format!("CAST({num} AS DOUBLE) / NULLIF({den}, 0)")
            }
            MetricKind::Derived { expr } => {
                // Shape-check; the walk below relies on balanced braces.
                derived_tokens(expr).map_err(SemanticError::Compile)?;
                let mut out = String::with_capacity(expr.len());
                let mut rest = expr.as_str();
                while let Some(start) = rest.find('{') {
                    let Some(len) = rest[start..].find('}') else {
                        break; // unreachable: balance validated above
                    };
                    out.push_str(&rest[..start]);
                    let body = &rest[start + 1..start + len];
                    let (member, filter) = match body.split_once('|') {
                        Some((m, f)) => (
                            m.trim(),
                            Some(f.trim().to_string()).filter(|s| !s.is_empty()),
                        ),
                        None => (body.trim(), None),
                    };
                    let sub =
                        self.member_expr(member, &and_filter(&base, filter.as_ref()), visiting)?;
                    out.push_str(&format!("({sub})"));
                    rest = &rest[start + len + 1..];
                }
                out.push_str(rest);
                out
            }
            MetricKind::Cumulative {
                measure,
                window,
                agg,
            } => {
                let col = self.leaf(measure, &base)?;
                let (time_col, mut partition) = self.window_axis(name)?;
                if let CumulativeWindow::GrainToDate { grain } = window {
                    partition.push(format!("DATE_TRUNC('{}', {time_col})", grain.as_str()));
                }
                let frame = match window {
                    CumulativeWindow::RunningTotal | CumulativeWindow::GrainToDate { .. } => {
                        "ROWS UNBOUNDED PRECEDING".to_string()
                    }
                    CumulativeWindow::Trailing { periods } => format!(
                        "ROWS BETWEEN {} PRECEDING AND CURRENT ROW",
                        periods.saturating_sub(1)
                    ),
                };
                let zero_fill = matches!(agg, WindowAgg::Sum | WindowAgg::Avg);
                format!(
                    "{}({}) OVER ({}ORDER BY {time_col} {frame})",
                    window_fn(*agg),
                    gap_filled(&col, zero_fill),
                    partition_by(&partition),
                )
            }
            MetricKind::Offset {
                measure,
                periods,
                output,
            } => {
                let col = self.leaf(measure, &base)?;
                let (time_col, partition) = self.window_axis(name)?;
                let cur = gap_filled(&col, true);
                let lag = format!(
                    "LAG({cur}, {periods}) OVER ({}ORDER BY {time_col})",
                    partition_by(&partition),
                );
                match output {
                    OffsetOutput::Value => lag,
                    OffsetOutput::Delta => format!("({cur} - {lag})"),
                    OffsetOutput::Growth => {
                        format!("CAST(({cur} - {lag}) AS DOUBLE) / NULLIF({lag}, 0)")
                    }
                }
            }
            MetricKind::Share { measure, over, agg } => {
                for dim in over {
                    if !self.q.dimensions.contains(dim) {
                        return Err(SemanticError::ShareDimNotGrouped {
                            metric: name.to_string(),
                            dim: dim.clone(),
                        });
                    }
                }
                let col = self.leaf(measure, &base)?;
                let col = quote_ident(&col);
                let partition: Vec<String> = over
                    .iter()
                    .map(|d| format!("{}.{}", self.dims_alias, quote_ident(d)))
                    .collect();
                format!(
                    "CAST({AGG_ALIAS}.{col} AS DOUBLE) / NULLIF({}({AGG_ALIAS}.{col}) OVER ({}), 0)",
                    window_fn(*agg),
                    partition_by(&partition).trim_end(),
                )
            }
        };
        visiting.pop();
        Ok(expr)
    }

    /// A reference inside a metric: a `model.measure` leaf column, or another
    /// metric inlined recursively.
    fn member_expr(
        &mut self,
        member: &str,
        filters: &[String],
        visiting: &mut Vec<String>,
    ) -> Result<String, SemanticError> {
        if member.contains('.') {
            let col = self.leaf(member, filters)?;
            Ok(format!("{AGG_ALIAS}.{}", quote_ident(&col)))
        } else {
            self.metric_expr(member, filters, visiting)
        }
    }

    /// Register the leaf for `member` under `filters`, returning the
    /// inner-query member (= output column) that carries it.
    fn leaf(&mut self, member: &str, filters: &[String]) -> Result<String, SemanticError> {
        let mut key_filters: Vec<String> = filters.to_vec();
        key_filters.sort();
        key_filters.dedup();
        let key = (member.to_string(), key_filters);
        if let Some(col) = self.leaf_index.get(&key) {
            return Ok(col.clone());
        }

        let inner_member = if filters.is_empty() {
            member.to_string()
        } else {
            let (model_name, measure_name) = member
                .split_once('.')
                .ok_or_else(|| SemanticError::UnknownMember(member.to_string()))?;
            let model = self
                .catalog
                .models
                .get(model_name)
                .ok_or_else(|| SemanticError::UnknownModel(model_name.to_string()))?;
            let measure = model
                .measures
                .iter()
                .find(|m| m.name == measure_name)
                .ok_or_else(|| SemanticError::UnknownMember(member.to_string()))?;

            let mut synthetic = measure.clone();
            synthetic.name = format!("__mf{}_{measure_name}", self.leaf_index.len());
            synthetic.filters.extend(key.1.iter().cloned());
            let inner_member = format!("{model_name}.{}", synthetic.name);
            self.synthetic
                .entry(model_name.to_string())
                .or_default()
                .push(synthetic);
            inner_member
        };

        self.leaves.push(inner_member.clone());
        self.leaf_index.insert(key, inner_member.clone());
        Ok(inner_member)
    }

    /// The inner query's catalog: affected models cloned with any synthetic
    /// measures grafted on.
    fn augmented_catalog(&self) -> SemanticCatalog {
        let mut models = self.catalog.models.clone();
        for (model_name, extra) in &self.synthetic {
            if let Some(model) = models.get(model_name) {
                let mut cloned: SemanticModel = (**model).clone();
                cloned.measures.extend(extra.iter().cloned());
                models.insert(model_name.clone(), Arc::new(cloned));
            }
        }
        SemanticCatalog {
            models,
            metrics: HashMap::new(),
            time_spine: None,
        }
    }

    /// The window's ordering column and partition terms (the non-time
    /// dimensions), both on the axis side.
    fn window_axis(&self, metric: &str) -> Result<(String, Vec<String>), SemanticError> {
        match self.time_dims.as_slice() {
            [(member, _)] => {
                let time_col = format!("{AXIS_ALIAS}.{}", quote_ident(member));
                let partition = self
                    .q
                    .dimensions
                    .iter()
                    .filter(|d| *d != member)
                    .map(|d| format!("{AXIS_ALIAS}.{}", quote_ident(d)))
                    .collect();
                Ok((time_col, partition))
            }
            [] => Err(SemanticError::MetricNeedsTimeGrain {
                metric: metric.to_string(),
            }),
            _ => Err(SemanticError::Compile(format!(
                "metric `{metric}`: group by exactly one time-grained dimension (found several)"
            ))),
        }
    }

    /// The dense time-axis CTE body. Bounded by the aggregate's own min/max —
    /// arbitrary bounds could sit off the grain and miss every bucket — and
    /// cross-joined with the distinct non-time keys so each series is dense.
    fn axis_sql(&self) -> Result<String, SemanticError> {
        let (time_member, grain) = match self.time_dims.as_slice() {
            [one] => one.clone(),
            _ => {
                return Err(SemanticError::Compile(
                    "window metrics need exactly one time-grained dimension".into(),
                ));
            }
        };
        let tc = quote_ident(&time_member);
        let spine = match &self.catalog.time_spine {
            None => format!(
                "SELECT unnest(generate_series((SELECT MIN({tc}) FROM {AGG_ALIAS}), \
                 (SELECT MAX({tc}) FROM {AGG_ALIAS}), INTERVAL '{}')) AS {tc}",
                grain_step(grain)
            ),
            Some(ts) => {
                let table = TableName::parse(&ts.source).ok_or_else(|| {
                    SemanticError::Compile(format!(
                        "time_spine source `{}` must be `source.table`",
                        ts.source
                    ))
                })?;
                let col = quote_ident(&ts.column);
                format!(
                    "SELECT DISTINCT DATE_TRUNC('{g}', {col}) AS {tc} \
                     FROM {schema}.{tbl} \
                     WHERE DATE_TRUNC('{g}', {col}) \
                     BETWEEN (SELECT MIN({tc}) FROM {AGG_ALIAS}) \
                     AND (SELECT MAX({tc}) FROM {AGG_ALIAS})",
                    g = grain.as_str(),
                    schema = quote_ident(&table.schema),
                    tbl = quote_ident(&table.table),
                )
            }
        };
        let keys: Vec<String> = self
            .q
            .dimensions
            .iter()
            .filter(|d| **d != time_member)
            .map(|d| quote_ident(d))
            .collect();
        if keys.is_empty() {
            return Ok(spine);
        }
        let axis_cols: Vec<String> = std::iter::once(format!("__t.{tc}"))
            .chain(keys.iter().map(|k| format!("__k.{k}")))
            .collect();
        Ok(format!(
            "SELECT {}\nFROM ({spine}) AS __t\nCROSS JOIN (SELECT DISTINCT {} FROM {AGG_ALIAS}) AS __k",
            axis_cols.join(", "),
            keys.join(", ")
        ))
    }
}

/// The measure's value on the dense axis: gap rows have no aggregate row, so
/// additive windows read them as zero (min/max must keep NULL).
fn gap_filled(col: &str, zero_fill: bool) -> String {
    let col = quote_ident(col);
    if zero_fill {
        format!("COALESCE({AGG_ALIAS}.{col}, 0)")
    } else {
        format!("{AGG_ALIAS}.{col}")
    }
}

fn partition_by(terms: &[String]) -> String {
    if terms.is_empty() {
        String::new()
    } else {
        format!("PARTITION BY {} ", terms.join(", "))
    }
}

fn and_filter(base: &[String], extra: Option<&String>) -> Vec<String> {
    let mut out = base.to_vec();
    if let Some(f) = extra {
        out.push(f.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use pawrly_core::semantic::{
        CumulativeWindow, Dimension, DimensionType, Measure, MeasureAgg, Metric, MetricKind,
        OffsetOutput, Operand, Relationship, RelationshipKind, SemanticOrder, SemanticQuery,
        TimeGrain, WindowAgg,
    };

    use crate::{SemanticCatalog, SemanticError};

    fn measure(name: &str, expr: &str, agg: MeasureAgg) -> Measure {
        Measure {
            name: name.into(),
            agg,
            expr: expr.into(),
            filters: vec![],
            format: None,
            description: None,
        }
    }

    fn orders_model() -> pawrly_core::semantic::SemanticModel {
        pawrly_core::semantic::SemanticModel {
            name: "orders".into(),
            description: None,
            source: "shop.orders".into(),
            primary_key: vec!["id".into()],
            dimensions: vec![
                Dimension {
                    name: "status".into(),
                    expr: "status".into(),
                    data_type: DimensionType::String,
                    time_grains: vec![],
                    description: None,
                },
                Dimension {
                    name: "order_date".into(),
                    expr: "ordered_at".into(),
                    data_type: DimensionType::Time,
                    time_grains: vec![TimeGrain::Day, TimeGrain::Month],
                    description: None,
                },
            ],
            measures: vec![
                measure("revenue", "total_amount", MeasureAgg::Sum),
                measure("cost", "cost_amount", MeasureAgg::Sum),
                measure("order_count", "id", MeasureAgg::CountDistinct),
            ],
            relationships: vec![Relationship {
                name: "customer".into(),
                kind: RelationshipKind::ManyToOne,
                target_model: "customers".into(),
                join_predicate: "this.customer_id = customers.id".into(),
            }],
            pre_aggregations: vec![],
            segments: vec![],
            safety: None,
        }
    }

    fn customers_model() -> pawrly_core::semantic::SemanticModel {
        pawrly_core::semantic::SemanticModel {
            name: "customers".into(),
            description: None,
            source: "crm.customers".into(),
            primary_key: vec!["id".into()],
            dimensions: vec![Dimension {
                name: "region".into(),
                expr: "region".into(),
                data_type: DimensionType::String,
                time_grains: vec![],
                description: None,
            }],
            measures: vec![measure("customer_count", "id", MeasureAgg::CountDistinct)],
            relationships: vec![],
            pre_aggregations: vec![],
            segments: vec![],
            safety: None,
        }
    }

    fn ratio(name: &str, num: &str, den: &str) -> Metric {
        Metric {
            name: name.into(),
            description: None,
            kind: MetricKind::Ratio {
                numerator: Operand {
                    member: num.into(),
                    filter: None,
                },
                denominator: Operand {
                    member: den.into(),
                    filter: None,
                },
            },
            filter: None,
            format: None,
        }
    }

    fn derived(name: &str, expr: &str) -> Metric {
        Metric {
            name: name.into(),
            description: None,
            kind: MetricKind::Derived { expr: expr.into() },
            filter: None,
            format: None,
        }
    }

    fn catalog(metrics: Vec<Metric>) -> SemanticCatalog {
        SemanticCatalog::new_with_metrics(vec![orders_model(), customers_model()], metrics)
    }

    fn q(measures: &[&str], dimensions: &[&str]) -> SemanticQuery {
        SemanticQuery {
            measures: measures.iter().map(|s| (*s).to_string()).collect(),
            dimensions: dimensions.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn same_root_ratio_projects_over_agg() {
        let cat = catalog(vec![ratio("aov", "orders.revenue", "orders.order_count")]);
        let sql = cat.compile_sql(&q(&["aov"], &["orders.status"])).unwrap();
        assert!(
            sql.contains(
                "CAST(__agg.\"orders.revenue\" AS DOUBLE) / \
                 NULLIF(__agg.\"orders.order_count\", 0) AS \"aov\""
            ),
            "{sql}"
        );
        assert!(
            sql.contains("SUM(total_amount) AS \"orders.revenue\""),
            "{sql}"
        );
        assert!(sql.contains("GROUP BY status"), "{sql}");
        // The internal leaves never surface in the outer projection.
        let outer = sql.lines().next().unwrap();
        assert!(!outer.contains("AS \"orders.order_count\""), "{outer}");
        assert!(!outer.contains("AS \"orders.revenue\""), "{outer}");
    }

    #[test]
    fn explicit_measure_and_metric_share_a_leaf() {
        let cat = catalog(vec![ratio("aov", "orders.revenue", "orders.order_count")]);
        let sql = cat
            .compile_sql(&q(&["aov", "orders.revenue"], &["orders.status"]))
            .unwrap();
        // One inner leaf feeds both the metric and the explicit projection.
        assert_eq!(sql.matches("SUM(total_amount)").count(), 1, "{sql}");
        let outer = sql.lines().next().unwrap();
        assert!(outer.contains("AS \"orders.revenue\""), "{outer}");
        assert!(outer.contains("AS \"aov\""), "{outer}");
    }

    #[test]
    fn cross_model_ratio_takes_aggregate_locality() {
        let cat = catalog(vec![ratio(
            "arpu",
            "orders.revenue",
            "customers.customer_count",
        )]);
        let sql = cat
            .compile_sql(&q(&["arpu"], &["customers.region"]))
            .unwrap();
        assert!(sql.contains("WITH \"_orders\" AS ("), "{sql}");
        assert!(sql.contains("\"_customers\" AS ("), "{sql}");
        assert!(sql.contains("FULL OUTER JOIN"), "{sql}");
        assert!(
            sql.contains(
                "CAST(__agg.\"orders.revenue\" AS DOUBLE) / \
                 NULLIF(__agg.\"customers.customer_count\", 0) AS \"arpu\""
            ),
            "{sql}"
        );
    }

    #[test]
    fn metric_filter_pushes_down_to_leaf() {
        let mut paid = ratio("paid_aov", "orders.revenue", "orders.order_count");
        paid.filter = Some("status = 'paid'".into());
        let sql = catalog(vec![paid])
            .compile_sql(&q(&["paid_aov"], &["orders.status"]))
            .unwrap();
        assert!(sql.contains("FILTER (WHERE"), "{sql}");
        assert!(sql.contains("status = 'paid'"), "{sql}");
        // Filtered leaves are synthetic columns, not the base measures.
        assert!(sql.contains("__mf"), "{sql}");
    }

    #[test]
    fn derived_per_token_filters_make_distinct_leaves() {
        let m = derived(
            "food_gross_profit",
            "{orders.revenue | category = 'food'} - {orders.cost | category = 'food'}",
        );
        let sql = catalog(vec![m])
            .compile_sql(&q(&["food_gross_profit"], &["orders.status"]))
            .unwrap();
        assert_eq!(sql.matches("category = 'food'").count(), 2, "{sql}");
        assert!(sql.contains(") - ("), "{sql}");
    }

    #[test]
    fn metric_over_metric_inlines() {
        let cat = catalog(vec![
            ratio("aov", "orders.revenue", "orders.order_count"),
            derived("aov_cents", "{aov} * 100"),
        ]);
        let sql = cat
            .compile_sql(&q(&["aov_cents"], &["orders.status"]))
            .unwrap();
        assert!(
            sql.contains(
                "(CAST(__agg.\"orders.revenue\" AS DOUBLE) / \
                 NULLIF(__agg.\"orders.order_count\", 0)) * 100"
            ),
            "{sql}"
        );
    }

    #[test]
    fn order_by_metric_and_limit_land_on_the_outer_select() {
        let cat = catalog(vec![ratio("aov", "orders.revenue", "orders.order_count")]);
        let mut query = q(&["aov"], &["orders.status"]);
        query.order_by = vec![SemanticOrder {
            member: "aov".into(),
            direction: pawrly_core::semantic::OrderDir::Desc,
        }];
        query.limit = Some(5);
        let sql = cat.compile_sql(&query).unwrap();
        let outer_tail = sql.rsplit("__agg").next().unwrap();
        assert!(outer_tail.contains("ORDER BY \"aov\" DESC"), "{sql}");
        assert!(outer_tail.contains("LIMIT 5"), "{sql}");
    }

    #[test]
    fn unknown_metric_cycle_and_unsupported_kinds_error() {
        let cat = catalog(vec![]);
        assert!(matches!(
            cat.compile_sql(&q(&["ghost"], &[])),
            Err(SemanticError::UnknownMetric(m)) if m == "ghost"
        ));

        let cat = catalog(vec![derived("a", "{b} + 1"), derived("b", "{a} + 1")]);
        assert!(matches!(
            cat.compile_sql(&q(&["a"], &["orders.status"])),
            Err(SemanticError::MetricCycle(_))
        ));

        assert!(matches!(
            catalog(vec![cumulative("running", CumulativeWindow::RunningTotal, WindowAgg::Sum)])
                .compile_sql(&q(&["running"], &["orders.status"])),
            Err(SemanticError::MetricNeedsTimeGrain { metric }) if metric == "running"
        ));
    }

    fn cumulative(name: &str, window: CumulativeWindow, agg: WindowAgg) -> Metric {
        Metric {
            name: name.into(),
            description: None,
            kind: MetricKind::Cumulative {
                measure: "orders.revenue".into(),
                window,
                agg,
            },
            filter: None,
            format: None,
        }
    }

    #[test]
    fn cumulative_running_joins_the_spine() {
        let cat = catalog(vec![cumulative(
            "running",
            CumulativeWindow::RunningTotal,
            WindowAgg::Sum,
        )]);
        let sql = cat
            .compile_sql(&q(&["running"], &["orders.order_date.month"]))
            .unwrap();
        assert!(sql.starts_with("WITH __agg AS ("), "{sql}");
        assert!(sql.contains("generate_series"), "{sql}");
        assert!(sql.contains("INTERVAL '1 month'"), "{sql}");
        assert!(sql.contains("LEFT JOIN __agg ON"), "{sql}");
        assert!(
            sql.contains(
                "SUM(COALESCE(__agg.\"orders.revenue\", 0)) OVER (ORDER BY \
                 __axis.\"orders.order_date.month\" ROWS UNBOUNDED PRECEDING) AS \"running\""
            ),
            "{sql}"
        );
    }

    #[test]
    fn grain_to_date_resets_and_trailing_frames() {
        let cat = catalog(vec![
            cumulative(
                "ytd",
                CumulativeWindow::GrainToDate {
                    grain: TimeGrain::Year,
                },
                WindowAgg::Sum,
            ),
            cumulative(
                "avg_3",
                CumulativeWindow::Trailing { periods: 3 },
                WindowAgg::Avg,
            ),
        ]);
        let sql = cat
            .compile_sql(&q(&["ytd", "avg_3"], &["orders.order_date.month"]))
            .unwrap();
        assert!(
            sql.contains("PARTITION BY DATE_TRUNC('year', __axis.\"orders.order_date.month\")"),
            "{sql}"
        );
        assert!(
            sql.contains(
                "AVG(COALESCE(__agg.\"orders.revenue\", 0)) OVER (ORDER BY \
                 __axis.\"orders.order_date.month\" ROWS BETWEEN 2 PRECEDING AND CURRENT ROW)"
            ),
            "{sql}"
        );
    }

    #[test]
    fn offset_growth_partitions_non_time_dims_on_a_dense_axis() {
        let mom = Metric {
            name: "mom".into(),
            description: None,
            kind: MetricKind::Offset {
                measure: "orders.revenue".into(),
                periods: 1,
                output: OffsetOutput::Growth,
            },
            filter: None,
            format: None,
        };
        let sql = catalog(vec![mom])
            .compile_sql(&q(&["mom"], &["orders.order_date.month", "orders.status"]))
            .unwrap();
        assert!(
            sql.contains(
                "LAG(COALESCE(__agg.\"orders.revenue\", 0), 1) OVER \
                 (PARTITION BY __axis.\"orders.status\" ORDER BY \
                 __axis.\"orders.order_date.month\")"
            ),
            "{sql}"
        );
        assert!(sql.contains("CAST(("), "{sql}");
        // Each status series is independently dense.
        assert!(
            sql.contains("CROSS JOIN (SELECT DISTINCT \"orders.status\" FROM __agg)"),
            "{sql}"
        );
    }

    #[test]
    fn share_windows_over_the_partition_without_a_spine() {
        let pct = Metric {
            name: "pct".into(),
            description: None,
            kind: MetricKind::Share {
                measure: "orders.revenue".into(),
                over: vec!["orders.status".into()],
                agg: WindowAgg::Sum,
            },
            filter: None,
            format: None,
        };
        let sql = catalog(vec![pct.clone()])
            .compile_sql(&q(&["pct"], &["orders.status"]))
            .unwrap();
        assert!(!sql.contains("generate_series"), "{sql}");
        assert!(
            sql.contains(
                "CAST(__agg.\"orders.revenue\" AS DOUBLE) / \
                 NULLIF(SUM(__agg.\"orders.revenue\") OVER \
                 (PARTITION BY __agg.\"orders.status\"), 0)"
            ),
            "{sql}"
        );

        // A grand-total share is `OVER ()`; an ungrouped `over` dim errors.
        let mut grand = pct.clone();
        grand.name = "grand".into();
        grand.kind = MetricKind::Share {
            measure: "orders.revenue".into(),
            over: vec![],
            agg: WindowAgg::Sum,
        };
        let sql = catalog(vec![grand])
            .compile_sql(&q(&["grand"], &["orders.status"]))
            .unwrap();
        assert!(sql.contains("OVER (), 0)"), "{sql}");

        assert!(matches!(
            catalog(vec![pct]).compile_sql(&q(&["pct"], &["orders.order_date.month"])),
            Err(SemanticError::ShareDimNotGrouped { dim, .. }) if dim == "orders.status"
        ));
    }

    #[test]
    fn declared_time_spine_replaces_generate_series() {
        let cat = catalog(vec![cumulative(
            "running",
            CumulativeWindow::RunningTotal,
            WindowAgg::Sum,
        )])
        .with_time_spine(Some(pawrly_core::semantic::TimeSpine {
            source: "shop.calendar".into(),
            column: "d".into(),
        }));
        let sql = cat
            .compile_sql(&q(&["running"], &["orders.order_date.month"]))
            .unwrap();
        assert!(!sql.contains("generate_series"), "{sql}");
        assert!(
            sql.contains("SELECT DISTINCT DATE_TRUNC('month', \"d\") FROM \"shop\".\"calendar\"")
                || sql.contains("FROM \"shop\".\"calendar\""),
            "{sql}"
        );
    }
}
