//! Semantic-layer compiler.
//!
//! [`SemanticCatalog`] holds the workspace's [`SemanticModel`]s and turns a
//! [`SemanticQuery`] into a SQL string that the existing engine executes
//! against the `pawrly` catalog. It compiles grouped dimensions and aggregated
//! measures with filters, time-grain (and time-zone) truncation, ordering, and
//! a limit; resolves relationships into joins across models; binds row-level
//! security predicates as safe literals; and matches queries against declared
//! pre-aggregations (see [`rollup`]). Pre-aggregation materialization is
//! handled by the cache layer.

#![doc(html_root_url = "https://docs.rs/pawrly-semantic")]

use std::collections::HashMap;
use std::sync::Arc;

mod metric;
pub mod rollup;

use pawrly_core::semantic::{
    Dimension, DimensionType, FilterOp, Measure, MeasureAgg, Metric, OrderDir, RelationshipKind,
    SemanticFilter, SemanticModel, SemanticModelDescription, SemanticModelInfo, SemanticOrder,
    SemanticQuery, TimeGrain, TimeSpine,
};
use pawrly_core::{EngineError, SafetyError, TableName};

/// Errors raised while compiling a [`SemanticQuery`].
#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    #[error("unknown model `{0}`")]
    UnknownModel(String),

    #[error("unknown member `{0}`")]
    UnknownMember(String),

    #[error("query selects no measures or dimensions")]
    EmptyQuery,

    #[error("measures span multiple root models {0:?}; measures must share one root")]
    AmbiguousRoot(Vec<String>),

    #[error("model `{model}` is not reachable from root `{root}` via any relationship")]
    UnreachableModel { root: String, model: String },

    /// `PAWRLY_SEMANTIC_FANOUT` — grouping or joining `measure` across `via`
    /// traverses a `one_to_many` edge, which would multiply the measure's rows
    /// and silently over-count. The query must be reshaped (drop the fan-out
    /// member, or aggregate that fact separately).
    #[error(
        "measure `{measure}` fans out through `{via}` (a one-to-many join would \
         multiply its rows and over-count); aggregate that fact separately"
    )]
    FanOut { measure: String, via: String },

    /// `PAWRLY_SEMANTIC_DISCONNECTED` — `member`'s model cannot be reached from
    /// any measure root via the declared relationships, so there is no
    /// non-arbitrary way to join it in.
    #[error("member `{member}` is not connected to any measure root via a declared relationship")]
    DisconnectedMember { member: String },

    /// `PAWRLY_SEMANTIC_AMBIGUOUS_PATH` — two equal-length join paths connect
    /// `from` and `to`; the model needs an explicit relationship to pick one.
    #[error(
        "ambiguous join path between `{from}` and `{to}`; \
         declare an explicit relationship to disambiguate"
    )]
    AmbiguousJoinPath { from: String, to: String },

    /// A query referenced a segment that no model defines.
    #[error("unknown segment `{0}` (expected `model.segment`)")]
    UnknownSegment(String),

    #[error("relationship cycle through `{0}`")]
    RelationshipCycle(String),

    #[error("relationship `{rel}` on `{model}` targets unknown model `{target}`")]
    UnknownRelationshipTarget {
        model: String,
        rel: String,
        target: String,
    },

    #[error("invalid time grain `{grain}` for dimension `{dim}`")]
    InvalidGrain { dim: String, grain: String },

    #[error("model `{model}` has invalid source `{source_table}` (expected `schema.table`)")]
    InvalidSource { model: String, source_table: String },

    /// A safety guard rail (required filter, RLS param) blocked the query. Kept
    /// as a distinct variant so it surfaces through `EngineError::Safety` with
    /// its original stable code rather than being flattened into a plan error.
    #[error(transparent)]
    Safety(#[from] SafetyError),

    /// `PAWRLY_SEMANTIC_UNKNOWN_METRIC` — a dot-free member matches no metric.
    #[error("unknown metric `{0}` (a dot-free member must name a configured metric)")]
    UnknownMetric(String),

    /// `PAWRLY_SEMANTIC_METRIC_CYCLE`
    #[error("metric reference cycle through `{0}`")]
    MetricCycle(String),

    /// `PAWRLY_SEMANTIC_METRIC_NEEDS_TIME` — a window metric was queried
    /// without exactly one time-grained dimension to order the window by.
    #[error(
        "metric `{metric}` is a window metric; group by exactly one time dimension \
         with a grain (e.g. `orders.order_date.month`)"
    )]
    MetricNeedsTimeGrain { metric: String },

    /// `PAWRLY_SEMANTIC_SHARE_DIM` — a `Share.over` dimension is not among the
    /// query's grouped dimensions, so its partition is undefined.
    #[error(
        "share metric `{metric}` partitions over `{dim}`, which is not among the \
         query's dimensions"
    )]
    ShareDimNotGrouped { metric: String, dim: String },

    #[error("compile failure: {0}")]
    Compile(String),
}

impl From<SemanticError> for EngineError {
    fn from(e: SemanticError) -> Self {
        match e {
            SemanticError::Safety(s) => EngineError::Safety(s),
            other => EngineError::SemanticPlan(other.to_string()),
        }
    }
}

/// The set of semantic models (and workspace metrics) defined in a workspace.
#[derive(Debug, Default, Clone)]
pub struct SemanticCatalog {
    pub(crate) models: HashMap<String, Arc<SemanticModel>>,
    pub(crate) metrics: HashMap<String, Arc<Metric>>,
    /// Declared calendar table for window metrics; absent = generated axis.
    pub(crate) time_spine: Option<TimeSpine>,
}

impl SemanticCatalog {
    /// Build a catalog from the configured models.
    #[must_use]
    pub fn new(models: Vec<SemanticModel>) -> Self {
        Self::new_with_metrics(models, Vec::new())
    }

    /// Build a catalog from the configured models and workspace metrics.
    #[must_use]
    pub fn new_with_metrics(models: Vec<SemanticModel>, metrics: Vec<Metric>) -> Self {
        let models = models
            .into_iter()
            .map(|m| (m.name.clone(), Arc::new(m)))
            .collect();
        let metrics = metrics
            .into_iter()
            .map(|m| (m.name.clone(), Arc::new(m)))
            .collect();
        Self {
            models,
            metrics,
            time_spine: None,
        }
    }

    /// Set the declared calendar table window metrics run over.
    #[must_use]
    pub fn with_time_spine(mut self, spine: Option<TimeSpine>) -> Self {
        self.time_spine = spine;
        self
    }

    /// True when no models are defined (the layer is effectively off).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Number of defined models.
    #[must_use]
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// List models as lightweight info rows, sorted by name for determinism.
    #[must_use]
    pub fn list(&self) -> Vec<SemanticModelInfo> {
        let mut out: Vec<SemanticModelInfo> = self
            .models
            .values()
            .map(|m| SemanticModelInfo {
                name: m.name.clone(),
                description: m.description.clone(),
                source: m.source.clone(),
                dimension_count: m.dimensions.len() as u32,
                measure_count: m.measures.len() as u32,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Full spec for one model, if it exists.
    #[must_use]
    pub fn describe(&self, name: &str) -> Option<SemanticModelDescription> {
        self.models.get(name).map(|m| SemanticModelDescription {
            name: m.name.clone(),
            description: m.description.clone(),
            source: m.source.clone(),
            primary_key: m.primary_key.clone(),
            dimensions: m.dimensions.clone(),
            measures: m.measures.clone(),
            relationships: m.relationships.clone(),
            segments: m.segments.clone(),
        })
    }

    /// Compile a query into a SQL string over the `pawrly` catalog.
    ///
    /// Queries with zero or one measure root compile to the original
    /// `FROM … JOIN … GROUP BY` form (unqualified and unaliased for a single
    /// model; aliased once joins are in play). Queries whose measures span two
    /// or more fact roots compile to **aggregate-locality** form: each
    /// fact is pre-aggregated at the shared-dimension grain in its own CTE and
    /// the CTEs are `FULL OUTER JOIN`-ed on the shared keys, so a `one_to_many`
    /// join can never inflate another fact's aggregate.
    ///
    /// A member that would fan out a measure (be reached across a `one_to_many`
    /// edge), one that is unreachable from a measure root, or an ambiguous join
    /// path are all rejected with a typed error rather than compiled to
    /// silently-wrong SQL.
    pub fn compile_sql(&self, q: &SemanticQuery) -> Result<String, SemanticError> {
        let q = self.expand_segments(q)?;
        let q = &q;

        if q.measures.is_empty() && q.dimensions.is_empty() {
            return Err(SemanticError::EmptyQuery);
        }

        // A dot-free measure member is a metric (`model.member` always dots);
        // those queries expand to leaf measures and re-enter this path.
        if q.measures.iter().any(|m| !m.contains('.')) {
            return metric::compile_with_metrics(self, q);
        }

        let measure_roots = self.measure_roots(q)?;
        if measure_roots.len() >= 2 {
            self.compile_aggregate_locality(q, &measure_roots)
        } else {
            let has_measures = !measure_roots.is_empty();
            let root = match measure_roots.into_iter().next() {
                Some(r) => r,
                None => member_model(q.dimensions.first().ok_or(SemanticError::EmptyQuery)?)?
                    .to_string(),
            };
            self.compile_single(q, &root, has_measures)
        }
    }

    /// Expand any `model.segment` references into their predicates, AND-ed in
    /// alongside the query's own filters. Returns the query unchanged when it
    /// selects no segments.
    fn expand_segments(&self, q: &SemanticQuery) -> Result<SemanticQuery, SemanticError> {
        if q.segments.is_empty() {
            return Ok(q.clone());
        }
        let mut out = q.clone();
        for seg_ref in &q.segments {
            let (model_name, seg_name) = seg_ref
                .split_once('.')
                .ok_or_else(|| SemanticError::UnknownSegment(seg_ref.clone()))?;
            let model = self
                .models
                .get(model_name)
                .ok_or_else(|| SemanticError::UnknownModel(model_name.to_string()))?;
            let seg = model
                .segments
                .iter()
                .find(|s| s.name == seg_name)
                .ok_or_else(|| SemanticError::UnknownSegment(seg_ref.clone()))?;
            out.filters.extend(seg.filters.iter().cloned());
        }
        out.segments.clear();
        Ok(out)
    }

    /// Distinct models owning the query's measures, in first-appearance order.
    fn measure_roots(&self, q: &SemanticQuery) -> Result<Vec<String>, SemanticError> {
        let mut roots: Vec<String> = Vec::new();
        for member in &q.measures {
            let model = member_model(member)?.to_string();
            if !self.models.contains_key(&model) {
                return Err(SemanticError::UnknownModel(model));
            }
            if !roots.contains(&model) {
                roots.push(model);
            }
        }
        Ok(roots)
    }

    /// Compile a single-fact query (`root` owns every measure, or there are no
    /// measures and `root` anchors the dimensions). Lookups (`many_to_one` /
    /// `one_to_one`) join in; a member reached across a `one_to_many` edge fans
    /// out the measure and is rejected.
    fn compile_single(
        &self,
        q: &SemanticQuery,
        root: &str,
        has_measures: bool,
    ) -> Result<String, SemanticError> {
        let referenced = self.referenced_models(q)?;
        let reach = self.reachability(root);
        // Validate connectivity, ambiguity, and (when aggregating) fan-out.
        for model in &referenced {
            if model == root {
                continue;
            }
            let node = reach
                .nodes
                .get(model)
                .ok_or_else(|| SemanticError::DisconnectedMember {
                    member: self.first_member_for_model(q, model),
                })?;
            if node.ambiguous {
                return Err(SemanticError::AmbiguousJoinPath {
                    from: root.to_string(),
                    to: model.clone(),
                });
            }
            if has_measures && node.fans_out {
                return Err(SemanticError::FanOut {
                    measure: self.first_measure_member(q, root),
                    via: model.clone(),
                });
            }
        }

        let joined = referenced.len() > 1;
        let tz = q.time_zone.as_deref();
        let alias_for = |model: &str| -> Option<String> { joined.then(|| model.to_string()) };

        let mut select_items: Vec<String> = Vec::new();
        let mut dim_exprs: Vec<String> = Vec::new();
        for member in &q.dimensions {
            let model = self.model_for_member(member)?;
            let expr =
                resolve_dimension_expr(model, member, alias_for(&model.name).as_deref(), tz)?;
            select_items.push(format!("{expr} AS {}", quote_ident(member)));
            dim_exprs.push(expr);
        }
        for member in &q.measures {
            let model = self.model_for_member(member)?;
            let expr = resolve_measure_expr(model, member, alias_for(&model.name).as_deref())?;
            select_items.push(format!("{expr} AS {}", quote_ident(member)));
        }

        let needed = self.needed_models(&reach, &referenced, root)?;
        let from = self.render_from(root, &reach, &needed, joined)?;

        // Safety guard rails on the user-supplied filters (root model).
        self.enforce_filter_safety(root, q)?;

        // Row-level filters (dimensions) → WHERE; aggregate-level filters
        // (measures) → HAVING.
        let mut wheres: Vec<String> = Vec::new();
        let mut havings: Vec<String> = Vec::new();
        for f in &q.filters {
            let model = self.model_for_member(&f.member)?;
            let alias = alias_for(&model.name);
            if member_is_measure(model, &f.member) {
                havings.push(resolve_measure_filter(model, f, alias.as_deref())?);
            } else {
                wheres.push(resolve_filter(model, f, alias.as_deref(), tz)?);
            }
        }
        // Required predicates (RLS + always-on filters) for every touched model.
        for model_name in &referenced {
            let Some(model) = self.models.get(model_name) else {
                continue;
            };
            if let Some(safety) = &model.safety {
                for pred in &safety.required_predicates {
                    wheres.push(bind_params(pred, &q.params, model_name)?);
                }
            }
        }

        let orders = self.resolve_orders(q)?;
        let limit = effective_limit(q.limit, self.max_rows(root));

        let mut sql = format!("SELECT {}\nFROM {from}", select_items.join(", "));
        if !wheres.is_empty() {
            sql.push_str("\nWHERE ");
            sql.push_str(&wheres.join(" AND "));
        }
        if !dim_exprs.is_empty() {
            sql.push_str("\nGROUP BY ");
            sql.push_str(&dim_exprs.join(", "));
        }
        if !havings.is_empty() {
            sql.push_str("\nHAVING ");
            sql.push_str(&havings.join(" AND "));
        }
        if !orders.is_empty() {
            sql.push_str("\nORDER BY ");
            sql.push_str(&orders.join(", "));
        }
        if let Some(limit) = limit {
            sql.push_str(&format!("\nLIMIT {limit}"));
        }
        Ok(sql)
    }

    /// Compile a multi-fact query in aggregate-locality form: one CTE per
    /// measure root, each pre-aggregated at the shared-dimension grain, joined
    /// `FULL OUTER` on the shared keys so no fact inflates another.
    fn compile_aggregate_locality(
        &self,
        q: &SemanticQuery,
        measure_roots: &[String],
    ) -> Result<String, SemanticError> {
        let tz = q.time_zone.as_deref();
        let shared_dims = &q.dimensions;

        // Split filters: dimension predicates apply inside every CTE (row-level);
        // measure predicates apply once at the outer level (aggregate-level).
        let mut dim_filters: Vec<&SemanticFilter> = Vec::new();
        let mut measure_filters: Vec<&SemanticFilter> = Vec::new();
        for f in &q.filters {
            let model = self.model_for_member(&f.member)?;
            if member_is_measure(model, &f.member) {
                measure_filters.push(f);
            } else {
                dim_filters.push(f);
            }
        }

        // Per-root safety guard rails, and the combined row cap (tightest wins).
        let mut cap: Option<u64> = None;
        for root in measure_roots {
            self.enforce_filter_safety(root, q)?;
            if let Some(c) = self.max_rows(root) {
                cap = Some(cap.map_or(c, |x| x.min(c)));
            }
        }

        let cte_name = |root: &str| quote_ident(&format!("_{root}"));
        let mut ctes: Vec<String> = Vec::new();
        for root in measure_roots {
            let reach = self.reachability(root);
            let alias_for = |model: &str| Some(model.to_string());

            // Every shared dimension and dimension-filter model must be a safe
            // (non-fan-out) lookup from this root.
            let mut referenced: Vec<String> = vec![root.clone()];
            let consider =
                |member: &str, referenced: &mut Vec<String>| -> Result<(), SemanticError> {
                    let m = member_model(member)?.to_string();
                    if !self.models.contains_key(&m) {
                        return Err(SemanticError::UnknownModel(m));
                    }
                    if m != *root {
                        let node = reach.nodes.get(&m).ok_or_else(|| {
                            SemanticError::DisconnectedMember {
                                member: member.to_string(),
                            }
                        })?;
                        if node.ambiguous {
                            return Err(SemanticError::AmbiguousJoinPath {
                                from: root.clone(),
                                to: m.clone(),
                            });
                        }
                        if node.fans_out {
                            return Err(SemanticError::FanOut {
                                measure: self.first_measure_member(q, root),
                                via: m.clone(),
                            });
                        }
                    }
                    if !referenced.contains(&m) {
                        referenced.push(m);
                    }
                    Ok(())
                };
            for d in shared_dims {
                consider(d, &mut referenced)?;
            }
            for f in &dim_filters {
                consider(&f.member, &mut referenced)?;
            }

            let needed = self.needed_models(&reach, &referenced, root)?;
            let from = self.render_from(root, &reach, &needed, true)?;

            let mut select_items: Vec<String> = Vec::new();
            let mut group_exprs: Vec<String> = Vec::new();
            for d in shared_dims {
                let model = self.model_for_member(d)?;
                let expr = resolve_dimension_expr(model, d, alias_for(&model.name).as_deref(), tz)?;
                select_items.push(format!("{expr} AS {}", quote_ident(d)));
                group_exprs.push(expr);
            }
            for m in q
                .measures
                .iter()
                .filter(|m| member_model(m).map(|mm| mm == root).unwrap_or(false))
            {
                let model = self.model_for_member(m)?;
                let expr = resolve_measure_expr(model, m, alias_for(&model.name).as_deref())?;
                select_items.push(format!("{expr} AS {}", quote_ident(m)));
            }

            let mut wheres: Vec<String> = Vec::new();
            for f in &dim_filters {
                let model = self.model_for_member(&f.member)?;
                wheres.push(resolve_filter(
                    model,
                    f,
                    alias_for(&model.name).as_deref(),
                    tz,
                )?);
            }
            // Required predicates (RLS) for every model this CTE touches — the
            // root *and* each lookup it joins to — so a fact aggregated through
            // a join still honors the joined model's row-level security.
            for model_name in &referenced {
                if let Some(safety) = self.models.get(model_name).and_then(|m| m.safety.as_ref()) {
                    for pred in &safety.required_predicates {
                        wheres.push(bind_params(pred, &q.params, model_name)?);
                    }
                }
            }

            let mut body = format!("SELECT {}\nFROM {from}", select_items.join(", "));
            if !wheres.is_empty() {
                body.push_str("\nWHERE ");
                body.push_str(&wheres.join(" AND "));
            }
            if !group_exprs.is_empty() {
                body.push_str("\nGROUP BY ");
                body.push_str(&group_exprs.join(", "));
            }
            ctes.push(format!("{} AS (\n{body}\n)", cte_name(root)));
        }

        // Outer SELECT: coalesce each shared dimension across all CTEs; pull
        // each measure from the CTE that owns it.
        let mut select_items: Vec<String> = Vec::new();
        for d in shared_dims {
            let col = quote_ident(d);
            let coalesced = measure_roots
                .iter()
                .map(|r| format!("{}.{col}", cte_name(r)))
                .collect::<Vec<_>>()
                .join(", ");
            select_items.push(format!("COALESCE({coalesced}) AS {col}"));
        }
        for m in &q.measures {
            let root = member_model(m)?;
            let col = quote_ident(m);
            select_items.push(format!("{}.{col} AS {col}", cte_name(root)));
        }

        // FULL OUTER JOIN the CTEs on the shared keys (CROSS JOIN when there are
        // no shared dimensions — each CTE is then a single grand-total row).
        let mut from = cte_name(&measure_roots[0]).to_string();
        for (i, root) in measure_roots.iter().enumerate().skip(1) {
            let name = cte_name(root);
            if shared_dims.is_empty() {
                from.push_str(&format!("\nCROSS JOIN {name}"));
                continue;
            }
            let conds = shared_dims
                .iter()
                .map(|d| {
                    let col = quote_ident(d);
                    let prev = measure_roots[..i]
                        .iter()
                        .map(|r| format!("{}.{col}", cte_name(r)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("COALESCE({prev}) IS NOT DISTINCT FROM {name}.{col}")
                })
                .collect::<Vec<_>>()
                .join(" AND ");
            from.push_str(&format!("\nFULL OUTER JOIN {name} ON {conds}"));
        }

        // Measure-threshold filters apply once, over the joined measures. They
        // reference the owning CTE's column (not the output alias, which a
        // `WHERE` cannot see).
        let mut wheres: Vec<String> = Vec::new();
        for f in &measure_filters {
            let root = member_model(&f.member)?;
            let col = format!("{}.{}", cte_name(root), quote_ident(&f.member));
            wheres.push(apply_op(&col, f, true)?);
        }

        let orders = self.resolve_orders(q)?;
        let limit = effective_limit(q.limit, cap);

        let mut sql = format!(
            "WITH {}\nSELECT {}\nFROM {from}",
            ctes.join(",\n"),
            select_items.join(", ")
        );
        if !wheres.is_empty() {
            sql.push_str("\nWHERE ");
            sql.push_str(&wheres.join(" AND "));
        }
        if !orders.is_empty() {
            sql.push_str("\nORDER BY ");
            sql.push_str(&orders.join(", "));
        }
        if let Some(limit) = limit {
            sql.push_str(&format!("\nLIMIT {limit}"));
        }
        Ok(sql)
    }

    /// Validate and render the ORDER BY terms; every term must name a selected
    /// member. Shared between the single and aggregate-locality paths.
    fn resolve_orders(&self, q: &SemanticQuery) -> Result<Vec<String>, SemanticError> {
        let selected: std::collections::HashSet<&str> = q
            .dimensions
            .iter()
            .chain(q.measures.iter())
            .map(String::as_str)
            .collect();
        let mut orders: Vec<String> = Vec::new();
        for o in &q.order_by {
            if !selected.contains(o.member.as_str()) {
                return Err(SemanticError::UnknownMember(o.member.clone()));
            }
            orders.push(resolve_order(o));
        }
        Ok(orders)
    }

    /// The first measure member owned by `root` (for fan-out diagnostics), or
    /// the root name when the query has no measures on it.
    fn first_measure_member(&self, q: &SemanticQuery, root: &str) -> String {
        q.measures
            .iter()
            .find(|m| member_model(m).map(|mm| mm == root).unwrap_or(false))
            .cloned()
            .unwrap_or_else(|| root.to_string())
    }

    /// The first query member whose model is `model` (for diagnostics).
    fn first_member_for_model(&self, q: &SemanticQuery, model: &str) -> String {
        q.measures
            .iter()
            .chain(q.dimensions.iter())
            .chain(q.filters.iter().map(|f| &f.member))
            .find(|m| member_model(m).map(|mm| mm == model).unwrap_or(false))
            .cloned()
            .unwrap_or_else(|| model.to_string())
    }

    /// The model that owns a member, resolved by its `model.` prefix.
    fn model_for_member(&self, member: &str) -> Result<&SemanticModel, SemanticError> {
        let name = member_model(member)?;
        self.models
            .get(name)
            .map(Arc::as_ref)
            .ok_or_else(|| SemanticError::UnknownModel(name.to_string()))
    }

    /// Distinct models referenced anywhere in the query, in first-appearance
    /// order (measures, then dimensions, then filters).
    fn referenced_models(&self, q: &SemanticQuery) -> Result<Vec<String>, SemanticError> {
        let mut out: Vec<String> = Vec::new();
        let members = q
            .measures
            .iter()
            .chain(q.dimensions.iter())
            .chain(q.filters.iter().map(|f| &f.member));
        for member in members {
            let name = member_model(member)?.to_string();
            if !self.models.contains_key(&name) {
                return Err(SemanticError::UnknownModel(name));
            }
            if !out.contains(&name) {
                out.push(name);
            }
        }
        Ok(out)
    }

    /// Build the fan-out-aware reachability map from `root`. Relationships are
    /// walked in both directions (a declared `one_to_many` is also a
    /// `many_to_one` from the child), so either fact can anchor a join. Each
    /// reached model records the cumulative fan-out of its shortest path and
    /// whether a second equal-length path makes it ambiguous.
    fn reachability(&self, root: &str) -> Reach {
        use std::collections::VecDeque;
        let adj = self.adjacency();
        let mut nodes: HashMap<String, Node> = HashMap::new();
        nodes.insert(root.to_string(), Node::root());
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(root.to_string());

        while let Some(cur) = queue.pop_front() {
            let (depth, fans_out) = {
                let n = &nodes[&cur];
                (n.depth, n.fans_out)
            };
            let Some(edges) = adj.get(&cur) else {
                continue;
            };
            for e in edges {
                let nd = depth + 1;
                let nf = fans_out || e.fans_out;
                match nodes.get(&e.target) {
                    None => {
                        nodes.insert(
                            e.target.clone(),
                            Node {
                                depth: nd,
                                fans_out: nf,
                                parent: Some(cur.clone()),
                                on: Some(e.on.clone()),
                                kind: Some(e.kind),
                                ambiguous: false,
                            },
                        );
                        queue.push_back(e.target.clone());
                    }
                    Some(existing) => {
                        // A second path of the same length via a different
                        // predecessor is an ambiguous join.
                        if existing.depth == nd && existing.parent.as_deref() != Some(cur.as_str())
                        {
                            if let Some(n) = nodes.get_mut(&e.target) {
                                n.ambiguous = true;
                            }
                        }
                    }
                }
            }
        }
        Reach { nodes }
    }

    /// The bidirectional relationship graph, one entry per `from` model. At most
    /// one edge is kept per `(from, target)` pair (declared edges win over
    /// inverted ones, in deterministic model order) so a relationship declared
    /// on both sides is not mistaken for two competing paths.
    fn adjacency(&self) -> HashMap<String, Vec<Edge>> {
        let mut adj: HashMap<String, Vec<Edge>> = HashMap::new();
        let mut push = |from: &str, edge: Edge| {
            let v = adj.entry(from.to_string()).or_default();
            if v.iter().any(|e| e.target == edge.target) {
                return;
            }
            v.push(edge);
        };
        let mut names: Vec<&String> = self.models.keys().collect();
        names.sort();
        for name in names {
            let model = &self.models[name];
            for rel in &model.relationships {
                if !self.models.contains_key(&rel.target_model) {
                    continue; // unknown targets are caught by config validation
                }
                let on = rel
                    .join_predicate
                    .replace("this.", &format!("{}.", model.name));
                push(
                    &model.name,
                    Edge {
                        target: rel.target_model.clone(),
                        kind: rel.kind,
                        on: on.clone(),
                        fans_out: matches!(rel.kind, RelationshipKind::OneToMany),
                    },
                );
                let inv = invert_kind(rel.kind);
                push(
                    &rel.target_model,
                    Edge {
                        target: model.name.clone(),
                        kind: inv,
                        on,
                        fans_out: matches!(inv, RelationshipKind::OneToMany),
                    },
                );
            }
        }
        adj
    }

    /// The non-root models that must be joined to connect `referenced` back to
    /// `root`, ordered parents-before-children (by reachability depth).
    fn needed_models(
        &self,
        reach: &Reach,
        referenced: &[String],
        root: &str,
    ) -> Result<Vec<String>, SemanticError> {
        use std::collections::HashSet;
        let mut set: HashSet<String> = HashSet::new();
        for model in referenced {
            if model == root {
                continue;
            }
            let mut cur = model.clone();
            while cur != root {
                let node = reach
                    .nodes
                    .get(&cur)
                    .ok_or_else(|| SemanticError::RelationshipCycle(cur.clone()))?;
                set.insert(cur.clone());
                cur = node
                    .parent
                    .clone()
                    .ok_or_else(|| SemanticError::RelationshipCycle(cur.clone()))?;
            }
        }
        let mut out: Vec<String> = set.into_iter().collect();
        out.sort_by_key(|m| reach.nodes.get(m).map(|n| n.depth).unwrap_or(u32::MAX));
        Ok(out)
    }

    /// `"schema"."table"` for a model's source, or an `InvalidSource` error.
    fn qualified_table(&self, model_name: &str) -> Result<String, SemanticError> {
        let model = self
            .models
            .get(model_name)
            .ok_or_else(|| SemanticError::UnknownModel(model_name.to_string()))?;
        let table =
            TableName::parse(&model.source).ok_or_else(|| SemanticError::InvalidSource {
                model: model.name.clone(),
                source_table: model.source.clone(),
            })?;
        Ok(format!(
            "{}.{}",
            quote_ident(&table.schema),
            quote_ident(&table.table)
        ))
    }

    /// Render the `FROM` (+ `JOIN`) clause from the reachability map. Unaliased
    /// for single-model queries to preserve the original output; aliased
    /// (`AS <model>`) when joining.
    fn render_from(
        &self,
        root: &str,
        reach: &Reach,
        needed: &[String],
        joined: bool,
    ) -> Result<String, SemanticError> {
        let root_table = self.qualified_table(root)?;
        if !joined {
            return Ok(root_table);
        }
        let mut from = format!("{root_table} AS {root}");
        for model in needed {
            let node = reach
                .nodes
                .get(model)
                .ok_or_else(|| SemanticError::RelationshipCycle(model.clone()))?;
            let (Some(kind), Some(on)) = (node.kind, node.on.as_ref()) else {
                return Err(SemanticError::RelationshipCycle(model.clone()));
            };
            let table = self.qualified_table(model)?;
            from.push_str(&format!(
                "\n{} JOIN {table} AS {model}\n  ON {on}",
                kind.join_kind()
            ));
        }
        Ok(from)
    }

    /// Enforce the root model's filter-presence guard rails against the
    /// user-supplied filters (run after compilation).
    fn enforce_filter_safety(&self, root: &str, q: &SemanticQuery) -> Result<(), SemanticError> {
        let Some(safety) = self.models.get(root).and_then(|m| m.safety.as_ref()) else {
            return Ok(());
        };
        if safety.require_at_least_one_filter && q.filters.is_empty() {
            return Err(SemanticError::Safety(SafetyError::NoFilters {
                table: root.to_string(),
            }));
        }
        for col in &safety.require_filters_on {
            let satisfied = q.filters.iter().any(|f| {
                member_model(&f.member).ok() == Some(root)
                    && dimension_of_member(&f.member) == Some(col.as_str())
            });
            if !satisfied {
                return Err(SemanticError::Safety(SafetyError::MissingRequiredFilter {
                    table: root.to_string(),
                    column: col.clone(),
                }));
            }
        }
        Ok(())
    }

    /// The root model's `max_rows` cap, if any.
    fn max_rows(&self, root: &str) -> Option<u64> {
        self.models
            .get(root)
            .and_then(|m| m.safety.as_ref())
            .and_then(|s| s.max_rows)
    }

    // ---- pre-aggregation materialization & rollup rewrite ----

    /// The materialization SQL for one pre-aggregation: the base table grouped
    /// at the pre-agg's own grain, each dimension stored under its bare name and
    /// each measure under its own name. No RLS or user filters — the full rollup
    /// is materialized once and row-level security is re-applied when a query
    /// reads it.
    pub fn compile_preagg_sql(&self, model: &str, preagg: &str) -> Result<String, SemanticError> {
        let m = self
            .models
            .get(model)
            .ok_or_else(|| SemanticError::UnknownModel(model.to_string()))?;
        let pre = m
            .pre_aggregations
            .iter()
            .find(|p| p.name == preagg)
            .ok_or_else(|| {
                SemanticError::Compile(format!("unknown pre-aggregation `{model}.{preagg}`"))
            })?;

        let mut select: Vec<String> = Vec::new();
        let mut group: Vec<String> = Vec::new();
        for d in &pre.dimensions {
            let bare = d.split('.').next().unwrap_or(d.as_str());
            let member = format!("{model}.{d}");
            let expr = resolve_dimension_expr(m, &member, None, None)?;
            select.push(format!("{expr} AS {}", quote_ident(bare)));
            group.push(expr);
        }
        for name in &pre.measures {
            let measure = find_measure(m, name)
                .ok_or_else(|| SemanticError::UnknownMember(format!("{model}.{name}")))?;
            select.push(format!(
                "{} AS {}",
                agg_sql(measure, None),
                quote_ident(name)
            ));
        }

        let from = self.qualified_table(model)?;
        let mut sql = format!("SELECT {}\nFROM {from}", select.join(", "));
        if !group.is_empty() {
            sql.push_str("\nGROUP BY ");
            sql.push_str(&group.join(", "));
        }
        Ok(sql)
    }

    /// Pick a pre-aggregation that can serve `q`, or `None` to read the base
    /// table. A rollup is used only when the query is single-model and
    /// single-fact, carries no RLS or time-zone handling, uses only additive
    /// measures, and a declared pre-agg covers it. Freshness is the caller's
    /// check (it consults the cache); a missing rollup never fails a query.
    #[must_use]
    pub fn candidate_rollup(&self, q: &SemanticQuery) -> Option<RollupMatch> {
        let q = self.expand_segments(q).ok()?;
        // The rollup is pre-truncated in its own zone; a tz query reads base.
        if q.time_zone.is_some() {
            return None;
        }
        let roots = self.measure_roots(&q).ok()?;
        let [root] = roots.as_slice() else {
            return None; // zero or multiple measure roots
        };
        let model = self.models.get(root)?;
        // No joins: every referenced member must belong to the one model.
        let referenced = self.referenced_models(&q).ok()?;
        if referenced.iter().any(|m| m != root) {
            return None;
        }
        // Conservative: a rollup would need to carry the RLS columns; skip.
        if model
            .safety
            .as_ref()
            .is_some_and(|s| !s.required_predicates.is_empty())
        {
            return None;
        }
        // Every measure must be re-aggregatable from a stored partial.
        for member in &q.measures {
            let name = measure_member_name(member)?;
            let measure = find_measure(model, name)?;
            if !crate::rollup::is_rollup_safe(&measure.agg) {
                return None;
            }
        }
        let pre = crate::rollup::match_rollup(model, &q)?;
        Some(RollupMatch {
            model: root.clone(),
            preagg: pre.name.clone(),
        })
    }

    /// Compile `q` to read the materialized rollup named by `r` instead of the
    /// base table: dimensions resolve to (re-truncated) rollup columns, measures
    /// re-aggregate the stored partials, and safety guard rails still apply.
    /// `r` must have come from [`Self::candidate_rollup`] for this query.
    pub fn compile_rollup_sql(
        &self,
        q: &SemanticQuery,
        r: &RollupMatch,
    ) -> Result<String, SemanticError> {
        let q = self.expand_segments(q)?;
        let model = self
            .models
            .get(&r.model)
            .ok_or_else(|| SemanticError::UnknownModel(r.model.clone()))?;
        let alias = r.model.as_str();
        let table = format!(
            "{}.{}",
            quote_ident(crate::rollup::ROLLUP_SCHEMA),
            quote_ident(&crate::rollup::rollup_table_name(&r.model, &r.preagg))
        );

        let mut select_items: Vec<String> = Vec::new();
        let mut dim_exprs: Vec<String> = Vec::new();
        for member in &q.dimensions {
            let expr = rollup_dim_expr(member, alias)?;
            select_items.push(format!("{expr} AS {}", quote_ident(member)));
            dim_exprs.push(expr);
        }
        for member in &q.measures {
            let name = measure_member_name(member)
                .ok_or_else(|| SemanticError::UnknownMember(member.clone()))?;
            let measure = find_measure(model, name)
                .ok_or_else(|| SemanticError::UnknownMember(member.clone()))?;
            let expr = combine_sql(&measure.agg, &rollup_col(alias, name));
            select_items.push(format!("{expr} AS {}", quote_ident(member)));
        }

        self.enforce_filter_safety(&r.model, &q)?;

        let mut wheres: Vec<String> = Vec::new();
        let mut havings: Vec<String> = Vec::new();
        for f in &q.filters {
            if member_is_measure(model, &f.member) {
                let name = measure_member_name(&f.member)
                    .ok_or_else(|| SemanticError::UnknownMember(f.member.clone()))?;
                let measure = find_measure(model, name)
                    .ok_or_else(|| SemanticError::UnknownMember(f.member.clone()))?;
                havings.push(apply_op(
                    &combine_sql(&measure.agg, &rollup_col(alias, name)),
                    f,
                    true,
                )?);
            } else {
                let expr = rollup_dim_expr(&f.member, alias)?;
                wheres.push(apply_op(&expr, f, false)?);
            }
        }

        let orders = self.resolve_orders(&q)?;
        let limit = effective_limit(q.limit, self.max_rows(&r.model));

        let mut sql = format!(
            "SELECT {}\nFROM {table} AS {alias}",
            select_items.join(", ")
        );
        if !wheres.is_empty() {
            sql.push_str("\nWHERE ");
            sql.push_str(&wheres.join(" AND "));
        }
        if !dim_exprs.is_empty() {
            sql.push_str("\nGROUP BY ");
            sql.push_str(&dim_exprs.join(", "));
        }
        if !havings.is_empty() {
            sql.push_str("\nHAVING ");
            sql.push_str(&havings.join(" AND "));
        }
        if !orders.is_empty() {
            sql.push_str("\nORDER BY ");
            sql.push_str(&orders.join(", "));
        }
        if let Some(limit) = limit {
            sql.push_str(&format!("\nLIMIT {limit}"));
        }
        Ok(sql)
    }
}

/// A pre-aggregation chosen to serve a query — see
/// [`SemanticCatalog::candidate_rollup`].
#[derive(Debug, Clone)]
pub struct RollupMatch {
    /// Owning model name.
    pub model: String,
    /// Pre-aggregation name.
    pub preagg: String,
}

impl RollupMatch {
    /// The schema the materialized rollup table lives in.
    #[must_use]
    pub fn schema(&self) -> &'static str {
        crate::rollup::ROLLUP_SCHEMA
    }

    /// The rollup's table name within [`Self::schema`].
    #[must_use]
    pub fn table(&self) -> String {
        crate::rollup::rollup_table_name(&self.model, &self.preagg)
    }
}

/// A declared pre-aggregation to materialize, with its refresh cadence — the
/// engine enumerates these at boot to register rollup tables.
#[derive(Debug, Clone)]
pub struct RollupSpec {
    pub model: String,
    pub preagg: String,
    /// Background refresh interval; `None` materializes once (manual refresh).
    pub refresh: Option<std::time::Duration>,
}

impl SemanticCatalog {
    /// Every declared pre-aggregation across all models, sorted for determinism.
    #[must_use]
    pub fn rollups(&self) -> Vec<RollupSpec> {
        let mut out: Vec<RollupSpec> = Vec::new();
        for m in self.models.values() {
            for p in &m.pre_aggregations {
                out.push(RollupSpec {
                    model: m.name.clone(),
                    preagg: p.name.clone(),
                    refresh: p.refresh,
                });
            }
        }
        out.sort_by(|a, b| (&a.model, &a.preagg).cmp(&(&b.model, &b.preagg)));
        out
    }
}

/// One directed edge in the [`Reach`] graph; a declared relationship yields a
/// forward edge and an inverted one.
struct Edge {
    /// Model this edge leads to.
    target: String,
    /// Cardinality in the direction of travel (drives the join keyword).
    kind: RelationshipKind,
    /// Join condition, with `this.` rewritten to the declaring model's alias.
    on: String,
    /// True when traversing this edge multiplies the source rows (`one_to_many`).
    fans_out: bool,
}

/// One reached model in a [`Reach`] map.
struct Node {
    depth: u32,
    /// Whether the shortest path to this model crosses any `one_to_many` edge.
    fans_out: bool,
    /// Predecessor on the shortest path (`None` only for the root).
    parent: Option<String>,
    /// Join condition to reach this model from its parent.
    on: Option<String>,
    /// Cardinality of the edge from the parent (drives the join keyword).
    kind: Option<RelationshipKind>,
    /// True when a second equal-length path makes the join ambiguous.
    ambiguous: bool,
}

impl Node {
    fn root() -> Self {
        Self {
            depth: 0,
            fans_out: false,
            parent: None,
            on: None,
            kind: None,
            ambiguous: false,
        }
    }
}

/// Fan-out-aware reachability from a root model.
struct Reach {
    nodes: HashMap<String, Node>,
}

/// Invert a relationship's cardinality for the reverse traversal.
fn invert_kind(kind: RelationshipKind) -> RelationshipKind {
    match kind {
        RelationshipKind::OneToMany => RelationshipKind::ManyToOne,
        RelationshipKind::ManyToOne => RelationshipKind::OneToMany,
        RelationshipKind::OneToOne => RelationshipKind::OneToOne,
    }
}

/// True when `member` (a two-part `model.name`) names a measure on `model`.
fn member_is_measure(model: &SemanticModel, member: &str) -> bool {
    let parts: Vec<&str> = member.split('.').collect();
    parts.len() == 2 && find_measure(model, parts[1]).is_some()
}

/// The measure name from a two-part `model.measure` member; `None` for a
/// grained or otherwise non-measure member.
fn measure_member_name(member: &str) -> Option<&str> {
    let parts: Vec<&str> = member.split('.').collect();
    (parts.len() == 2).then(|| parts[1])
}

/// `alias."col"` — a qualified rollup column reference.
fn rollup_col(alias: &str, col: &str) -> String {
    format!("{alias}.{}", quote_ident(col))
}

/// Resolve a query dimension member against its rollup column, re-truncating to
/// the requested grain. The rollup stores the dimension under its bare name at
/// the pre-agg's (finer-or-equal) grain, so a coarser grain is a valid
/// re-`DATE_TRUNC`; an ungrained member reads the stored column directly.
fn rollup_dim_expr(member: &str, alias: &str) -> Result<String, SemanticError> {
    let parts: Vec<&str> = member.split('.').collect();
    let (dim, grain) = match parts.as_slice() {
        [_model, dim] => (*dim, None),
        [_model, dim, grain] => (*dim, Some(*grain)),
        _ => return Err(SemanticError::UnknownMember(member.to_string())),
    };
    let col = rollup_col(alias, dim);
    match grain {
        None => Ok(col),
        Some(g) => {
            let grain = TimeGrain::parse(g).ok_or_else(|| SemanticError::InvalidGrain {
                dim: dim.to_string(),
                grain: g.to_string(),
            })?;
            Ok(format!("DATE_TRUNC('{}', {col})", grain.as_str()))
        }
    }
}

/// The re-aggregation of a rolled-up partial: `SUM`/`COUNT` both sum the stored
/// per-group partials, `MIN`/`MAX` extend them. Only invoked for rollup-safe
/// aggregates (see [`crate::rollup::is_rollup_safe`]); non-additive variants
/// fall back to `SUM` rather than panicking.
fn combine_sql(agg: &MeasureAgg, col: &str) -> String {
    match agg {
        MeasureAgg::Min => format!("MIN({col})"),
        MeasureAgg::Max => format!("MAX({col})"),
        _ => format!("SUM({col})"),
    }
}

/// `"status"` from `"orders.status"` (the dimension segment of a member).
fn dimension_of_member(member: &str) -> Option<&str> {
    member.split('.').nth(1)
}

/// Clamp a requested limit to a `max_rows` cap; the cap also acts as a default
/// limit when the query is otherwise unbounded.
fn effective_limit(requested: Option<u64>, cap: Option<u64>) -> Option<u64> {
    match (requested, cap) {
        (Some(l), Some(c)) => Some(l.min(c)),
        (Some(l), None) => Some(l),
        (None, cap) => cap,
    }
}

/// Substitute `${param:NAME}` placeholders in a trusted predicate with the
/// bound value as an **escaped SQL literal** — never as a SQL fragment. A
/// value like `x' OR '1'='1` becomes the literal `'x'' OR ''1''=''1'`, which
/// compares as one string and cannot alter the query structure. An unbound
/// param is refused before any scan with [`SafetyError::UnboundParam`].
fn bind_params(
    pred: &str,
    params: &HashMap<String, String>,
    model: &str,
) -> Result<String, SafetyError> {
    const OPEN: &str = "${param:";
    let mut out = String::with_capacity(pred.len());
    let mut rest = pred;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let end = after
            .find('}')
            .ok_or_else(|| SafetyError::PredicateUnsatisfied {
                table: model.to_string(),
                predicate: pred.to_string(),
                reason: "unterminated ${param:...} placeholder".into(),
            })?;
        let name = &after[..end];
        let value = params.get(name).ok_or_else(|| SafetyError::UnboundParam {
            table: model.to_string(),
            name: name.to_string(),
        })?;
        out.push_str(&lit(value));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Extract the model prefix (`orders` from `orders.revenue`).
fn member_model(member: &str) -> Result<&str, SemanticError> {
    member
        .split('.')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| SemanticError::UnknownMember(member.to_string()))
}

/// Qualify a bare-column expression with a table alias when one is in play.
/// Non-identifier expressions (`total * 1.1`, function calls) are returned
/// verbatim — qualifying them is the author's responsibility in joined models.
fn qualify(expr: &str, alias: Option<&str>) -> String {
    match alias {
        Some(a) if is_bare_ident(expr) => format!("{a}.{expr}"),
        _ => expr.to_string(),
    }
}

/// True for a single unquoted SQL identifier (`status`, `total_amount`).
fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Resolve a dimension member to its SQL expression — qualified by `alias`
/// when joining, wrapped in `DATE_TRUNC` (in `tz` when supplied) for a grain.
/// Used by SELECT, GROUP BY (via the caller), and WHERE.
fn resolve_dimension_expr(
    model: &SemanticModel,
    member: &str,
    alias: Option<&str>,
    tz: Option<&str>,
) -> Result<String, SemanticError> {
    let parts: Vec<&str> = member.split('.').collect();
    // parts[0] is the (already-validated) model name.
    let (dim_name, grain) = match parts.as_slice() {
        [_model, dim] => (*dim, None),
        [_model, dim, grain] => (*dim, Some(*grain)),
        _ => return Err(SemanticError::UnknownMember(member.to_string())),
    };

    let dim = find_dimension(model, dim_name)
        .ok_or_else(|| SemanticError::UnknownMember(member.to_string()))?;
    let base = qualify(&dim.expr, alias);

    let Some(grain_str) = grain else {
        return Ok(base);
    };

    let grain = TimeGrain::parse(grain_str).ok_or_else(|| SemanticError::InvalidGrain {
        dim: dim_name.to_string(),
        grain: grain_str.to_string(),
    })?;
    if dim.data_type != DimensionType::Time {
        return Err(SemanticError::InvalidGrain {
            dim: dim_name.to_string(),
            grain: grain_str.to_string(),
        });
    }
    if !dim.time_grains.is_empty() && !dim.time_grains.contains(&grain) {
        return Err(SemanticError::InvalidGrain {
            dim: dim_name.to_string(),
            grain: grain_str.to_string(),
        });
    }
    // Truncate in the requested zone when one is given so day/week/month
    // boundaries land on local time rather than UTC.
    let trunc_input = match tz {
        Some(zone) => format!("{base} AT TIME ZONE {}", lit(zone)),
        None => base,
    };
    Ok(format!("DATE_TRUNC('{}', {trunc_input})", grain.as_str()))
}

/// Resolve a measure member to its aggregate SQL expression, qualifying the
/// aggregated column with `alias` when joining.
fn resolve_measure_expr(
    model: &SemanticModel,
    member: &str,
    alias: Option<&str>,
) -> Result<String, SemanticError> {
    let parts: Vec<&str> = member.split('.').collect();
    let measure_name = match parts.as_slice() {
        [_model, measure] => *measure,
        _ => return Err(SemanticError::UnknownMember(member.to_string())),
    };
    let measure = find_measure(model, measure_name)
        .ok_or_else(|| SemanticError::UnknownMember(member.to_string()))?;
    Ok(agg_sql(measure, alias))
}

fn find_dimension<'a>(model: &'a SemanticModel, name: &str) -> Option<&'a Dimension> {
    model.dimensions.iter().find(|d| d.name == name)
}

fn find_measure<'a>(model: &'a SemanticModel, name: &str) -> Option<&'a Measure> {
    model.measures.iter().find(|m| m.name == name)
}

/// `expr` and `filters` are raw SQL fragments authored in the trusted config,
/// so they are inlined directly (same trust model as the SQL surface). A
/// bare-column `expr` is qualified with `alias` when joining; a `Custom` SQL
/// aggregate is emitted verbatim (the author qualifies it).
fn agg_sql(m: &Measure, alias: Option<&str>) -> String {
    let col = qualify(&m.expr, alias);
    let base = match &m.agg {
        MeasureAgg::Sum => format!("SUM({col})"),
        MeasureAgg::Count => format!("COUNT({col})"),
        MeasureAgg::CountDistinct => format!("COUNT(DISTINCT {col})"),
        MeasureAgg::Avg => format!("AVG({col})"),
        MeasureAgg::Min => format!("MIN({col})"),
        MeasureAgg::Max => format!("MAX({col})"),
        MeasureAgg::Custom { sql } => sql.clone(),
    };
    if m.filters.is_empty() {
        base
    } else {
        format!("{base} FILTER (WHERE {})", m.filters.join(" AND "))
    }
}

/// A row-level (dimension) filter, resolved to a `WHERE` predicate.
fn resolve_filter(
    model: &SemanticModel,
    f: &SemanticFilter,
    alias: Option<&str>,
    tz: Option<&str>,
) -> Result<String, SemanticError> {
    let expr = resolve_dimension_expr(model, &f.member, alias, tz)?;
    apply_op(&expr, f, false)
}

/// An aggregate-level (measure) filter, resolved to a predicate over the
/// measure's aggregate expression — a `HAVING` term in single-fact form, or an
/// outer filter over the joined CTEs in aggregate-locality form. A measure is
/// numeric, so values compare numerically rather than as strings.
fn resolve_measure_filter(
    model: &SemanticModel,
    f: &SemanticFilter,
    alias: Option<&str>,
) -> Result<String, SemanticError> {
    let expr = resolve_measure_expr(model, &f.member, alias)?;
    apply_op(&expr, f, true)
}

/// Build a SQL predicate applying filter `f`'s operator to the already-resolved
/// `expr`. Values are bound as escaped literals. When `numeric` is set, a value
/// that parses as a number is emitted as a bare numeric literal so the
/// comparison is numeric (a measure threshold) rather than lexical; anything
/// that does not parse as a number still falls back to a safe quoted string.
fn apply_op(expr: &str, f: &SemanticFilter, numeric: bool) -> Result<String, SemanticError> {
    let one = |member: &str| -> Result<&str, SemanticError> {
        f.values
            .first()
            .map(String::as_str)
            .ok_or_else(|| SemanticError::Compile(format!("filter on `{member}` requires a value")))
    };
    let v = |s: &str| scalar_lit(s, numeric);
    let predicate = match f.op {
        FilterOp::Equals => format!("{expr} = {}", v(one(&f.member)?)),
        FilterOp::NotEquals => format!("{expr} <> {}", v(one(&f.member)?)),
        FilterOp::Gt => format!("{expr} > {}", v(one(&f.member)?)),
        FilterOp::Gte => format!("{expr} >= {}", v(one(&f.member)?)),
        FilterOp::Lt => format!("{expr} < {}", v(one(&f.member)?)),
        FilterOp::Lte => format!("{expr} <= {}", v(one(&f.member)?)),
        FilterOp::In | FilterOp::NotIn => {
            if f.values.is_empty() {
                return Err(SemanticError::Compile(format!(
                    "filter on `{}` requires at least one value",
                    f.member
                )));
            }
            let list = f
                .values
                .iter()
                .map(|x| scalar_lit(x, numeric))
                .collect::<Vec<_>>()
                .join(", ");
            let op = if matches!(f.op, FilterOp::In) {
                "IN"
            } else {
                "NOT IN"
            };
            format!("{expr} {op} ({list})")
        }
        FilterOp::InRange => {
            let lo = f.values.first().map(String::as_str);
            let hi = f.values.get(1).map(String::as_str);
            match (lo, hi) {
                (Some(lo), Some(hi)) => format!("{expr} BETWEEN {} AND {}", v(lo), v(hi)),
                _ => {
                    return Err(SemanticError::Compile(format!(
                        "in_range filter on `{}` requires two values",
                        f.member
                    )));
                }
            }
        }
        // LIKE operators are inherently string-valued, so they always quote.
        FilterOp::Contains => format!("{expr} LIKE {}", lit(&format!("%{}%", one(&f.member)?))),
        FilterOp::StartsWith => format!("{expr} LIKE {}", lit(&format!("{}%", one(&f.member)?))),
        FilterOp::EndsWith => format!("{expr} LIKE {}", lit(&format!("%{}", one(&f.member)?))),
        FilterOp::IsNull => format!("{expr} IS NULL"),
        FilterOp::IsNotNull => format!("{expr} IS NOT NULL"),
    };
    Ok(predicate)
}

/// A scalar literal: a bare numeric literal when `numeric` is set and the value
/// parses as an integer or float, otherwise a safely-quoted string. A value
/// that does not parse as a number can never escape quoting, so this stays
/// injection-safe.
fn scalar_lit(s: &str, numeric: bool) -> String {
    if numeric && (s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok()) {
        s.to_string()
    } else {
        lit(s)
    }
}

fn resolve_order(o: &SemanticOrder) -> String {
    let dir = match o.direction {
        OrderDir::Asc => "ASC",
        OrderDir::Desc => "DESC",
    };
    format!("{} {dir}", quote_ident(&o.member))
}

/// Quote a SQL identifier, escaping embedded double quotes.
fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Quote a SQL string literal, escaping embedded single quotes.
fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_core::safety::SafetyPolicy;
    use pawrly_core::semantic::{
        Dimension, DimensionType, Measure, MeasureAgg, PreAggregation, Relationship,
        RelationshipKind, TimeGrain,
    };

    fn orders_model() -> SemanticModel {
        SemanticModel {
            name: "orders".into(),
            description: Some("One row per order".into()),
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
                Measure {
                    name: "revenue".into(),
                    agg: MeasureAgg::Sum,
                    expr: "total_amount".into(),
                    filters: vec![],
                    format: None,
                    description: None,
                },
                Measure {
                    name: "paid_revenue".into(),
                    agg: MeasureAgg::Sum,
                    expr: "total_amount".into(),
                    filters: vec!["status = 'paid'".into()],
                    format: None,
                    description: None,
                },
            ],
            relationships: vec![],
            pre_aggregations: vec![],
            segments: vec![],
            safety: None,
        }
    }

    fn catalog() -> SemanticCatalog {
        SemanticCatalog::new(vec![orders_model()])
    }

    #[test]
    fn compiles_grouped_aggregate() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status".into()],
            ..Default::default()
        };
        let sql = catalog().compile_sql(&q).unwrap();
        assert!(
            sql.contains("SUM(total_amount) AS \"orders.revenue\""),
            "{sql}"
        );
        assert!(sql.contains("status AS \"orders.status\""), "{sql}");
        assert!(sql.contains("FROM \"shop\".\"orders\""), "{sql}");
        assert!(sql.contains("GROUP BY status"), "{sql}");
    }

    #[test]
    fn applies_time_grain() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.order_date.month".into()],
            ..Default::default()
        };
        let sql = catalog().compile_sql(&q).unwrap();
        assert!(
            sql.contains("DATE_TRUNC('month', ordered_at) AS \"orders.order_date.month\""),
            "{sql}"
        );
    }

    #[test]
    fn measure_filter_becomes_filter_clause() {
        let q = SemanticQuery {
            measures: vec!["orders.paid_revenue".into()],
            ..Default::default()
        };
        let sql = catalog().compile_sql(&q).unwrap();
        assert!(
            sql.contains("SUM(total_amount) FILTER (WHERE status = 'paid')"),
            "{sql}"
        );
    }

    #[test]
    fn where_filter_escapes_quotes() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status".into()],
            filters: vec![SemanticFilter {
                member: "orders.status".into(),
                op: FilterOp::Equals,
                values: vec!["o'brien".into()],
            }],
            ..Default::default()
        };
        let sql = catalog().compile_sql(&q).unwrap();
        assert!(sql.contains("WHERE status = 'o''brien'"), "{sql}");
    }

    #[test]
    fn unknown_member_errors() {
        let q = SemanticQuery {
            measures: vec!["orders.nope".into()],
            ..Default::default()
        };
        assert!(matches!(
            catalog().compile_sql(&q),
            Err(SemanticError::UnknownMember(_))
        ));
    }

    #[test]
    fn two_unrelated_totals_cross_join() {
        // Measures drawn from two unrelated fact roots with no shared dimension
        // are two independent grand totals: each aggregates in its own CTE and
        // the CTEs cross-join into a single row. (Formerly an `AmbiguousRoot`.)
        let mut customers = customer_model();
        customers.measures = vec![Measure {
            name: "customer_count".into(),
            agg: MeasureAgg::CountDistinct,
            expr: "id".into(),
            filters: vec![],
            format: None,
            description: None,
        }];
        let cat = SemanticCatalog::new(vec![orders_model(), customers]);
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into(), "customers.customer_count".into()],
            ..Default::default()
        };
        let sql = cat.compile_sql(&q).unwrap();
        assert!(sql.starts_with("WITH "), "{sql}");
        assert!(sql.contains("\"_orders\" AS ("), "{sql}");
        assert!(sql.contains("\"_customers\" AS ("), "{sql}");
        assert!(sql.contains("CROSS JOIN \"_customers\""), "{sql}");
        assert!(
            sql.contains("\"_orders\".\"orders.revenue\" AS \"orders.revenue\""),
            "{sql}"
        );
    }

    #[test]
    fn dimension_on_unknown_model_errors() {
        // A dimension that names a model that doesn't exist is an unknown model.
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["nosuch.region".into()],
            ..Default::default()
        };
        assert!(matches!(
            catalog().compile_sql(&q),
            Err(SemanticError::UnknownModel(_))
        ));
    }

    #[test]
    fn empty_query_errors() {
        assert!(matches!(
            catalog().compile_sql(&SemanticQuery::default()),
            Err(SemanticError::EmptyQuery)
        ));
    }

    #[test]
    fn bad_grain_errors() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status.month".into()],
            ..Default::default()
        };
        assert!(matches!(
            catalog().compile_sql(&q),
            Err(SemanticError::InvalidGrain { .. })
        ));
    }

    // ---- time zone ----

    #[test]
    fn time_zone_wraps_truncation() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.order_date.day".into()],
            time_zone: Some("America/New_York".into()),
            ..Default::default()
        };
        let sql = catalog().compile_sql(&q).unwrap();
        assert!(
            sql.contains("DATE_TRUNC('day', ordered_at AT TIME ZONE 'America/New_York')"),
            "{sql}"
        );
    }

    // ---- order by validation ----

    #[test]
    fn order_by_selected_member_ok() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status".into()],
            order_by: vec![SemanticOrder {
                member: "orders.revenue".into(),
                direction: OrderDir::Desc,
            }],
            ..Default::default()
        };
        let sql = catalog().compile_sql(&q).unwrap();
        assert!(sql.contains("ORDER BY \"orders.revenue\" DESC"), "{sql}");
    }

    #[test]
    fn order_by_unselected_member_errors() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status".into()],
            order_by: vec![SemanticOrder {
                member: "orders.order_date".into(), // not selected
                direction: OrderDir::Asc,
            }],
            ..Default::default()
        };
        assert!(matches!(
            catalog().compile_sql(&q),
            Err(SemanticError::UnknownMember(_))
        ));
    }

    // ---- joins ----

    fn customer_model() -> SemanticModel {
        SemanticModel {
            name: "customers".into(),
            description: None,
            source: "crm.dim_customers".into(),
            primary_key: vec!["id".into()],
            dimensions: vec![Dimension {
                name: "region".into(),
                expr: "region".into(),
                data_type: DimensionType::String,
                time_grains: vec![],
                description: None,
            }],
            measures: vec![],
            relationships: vec![],
            pre_aggregations: vec![],
            segments: vec![],
            safety: None,
        }
    }

    fn joined_catalog() -> SemanticCatalog {
        let mut orders = orders_model();
        orders.relationships = vec![Relationship {
            name: "customer".into(),
            kind: RelationshipKind::ManyToOne,
            target_model: "customers".into(),
            join_predicate: "this.customer_id = customers.id".into(),
        }];
        SemanticCatalog::new(vec![orders, customer_model()])
    }

    #[test]
    fn cross_model_query_emits_qualified_join() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["customers.region".into()],
            ..Default::default()
        };
        let sql = joined_catalog().compile_sql(&q).unwrap();
        // Aliased tables, qualified columns, INNER join (many_to_one).
        assert!(sql.contains("FROM \"shop\".\"orders\" AS orders"), "{sql}");
        assert!(
            sql.contains("INNER JOIN \"crm\".\"dim_customers\" AS customers"),
            "{sql}"
        );
        assert!(
            sql.contains("ON orders.customer_id = customers.id"),
            "{sql}"
        );
        assert!(
            sql.contains("SUM(orders.total_amount) AS \"orders.revenue\""),
            "{sql}"
        );
        assert!(
            sql.contains("customers.region AS \"customers.region\""),
            "{sql}"
        );
        assert!(sql.contains("GROUP BY customers.region"), "{sql}");
    }

    /// A model with a `one_to_many` edge to `customers`: each order maps to
    /// many customers (a deliberately fan-out-shaped relationship).
    fn one_to_many_catalog() -> SemanticCatalog {
        let mut orders = orders_model();
        orders.relationships = vec![Relationship {
            name: "customer".into(),
            kind: RelationshipKind::OneToMany,
            target_model: "customers".into(),
            join_predicate: "this.customer_id = customers.id".into(),
        }];
        SemanticCatalog::new(vec![orders, customer_model()])
    }

    #[test]
    fn one_to_many_dimension_fans_out_measure() {
        // Grouping a measure by a dimension reached across a `one_to_many` edge
        // would multiply the measure's rows — reject rather than over-count.
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["customers.region".into()],
            ..Default::default()
        };
        match one_to_many_catalog().compile_sql(&q) {
            Err(SemanticError::FanOut { measure, via }) => {
                assert_eq!(measure, "orders.revenue");
                assert_eq!(via, "customers");
            }
            other => panic!("expected FanOut, got {other:?}"),
        }
    }

    #[test]
    fn one_to_many_dimension_without_measure_left_joins() {
        // With no measure to inflate, the same `one_to_many` edge is harmless
        // and still compiles to a LEFT JOIN (join-kind coverage).
        let q = SemanticQuery {
            dimensions: vec!["orders.status".into(), "customers.region".into()],
            ..Default::default()
        };
        let sql = one_to_many_catalog().compile_sql(&q).unwrap();
        assert!(sql.contains("LEFT JOIN \"crm\".\"dim_customers\""), "{sql}");
    }

    #[test]
    fn unrelated_model_errors() {
        // customers has no relationship back, and orders has none here.
        let cat = SemanticCatalog::new(vec![orders_model(), customer_model()]);
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["customers.region".into()],
            ..Default::default()
        };
        assert!(matches!(
            cat.compile_sql(&q),
            Err(SemanticError::DisconnectedMember { .. })
        ));
    }

    #[test]
    fn single_model_output_stays_unaliased() {
        // Regression: a relationship on the model must not alias single-model
        // queries (preserves the original SQL shape).
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status".into()],
            ..Default::default()
        };
        let sql = joined_catalog().compile_sql(&q).unwrap();
        assert!(sql.contains("FROM \"shop\".\"orders\"\n"), "{sql}");
        assert!(!sql.contains(" AS orders"), "{sql}");
        assert!(sql.contains("GROUP BY status"), "{sql}");
    }

    // ---- RLS / required predicates ----

    fn rls_catalog() -> SemanticCatalog {
        let mut orders = orders_model();
        orders.safety = Some(SafetyPolicy {
            required_predicates: vec![
                "region = 'US'".into(),
                "tenant_id = ${param:tenant_id}".into(),
            ],
            ..Default::default()
        });
        SemanticCatalog::new(vec![orders])
    }

    #[test]
    fn required_predicates_bound_with_literal() {
        let mut q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            ..Default::default()
        };
        q.params.insert("tenant_id".into(), "acme".into());
        let sql = rls_catalog().compile_sql(&q).unwrap();
        assert!(sql.contains("region = 'US'"), "{sql}");
        assert!(sql.contains("tenant_id = 'acme'"), "{sql}");
    }

    #[test]
    fn unbound_param_is_refused() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            ..Default::default()
        };
        match rls_catalog().compile_sql(&q) {
            Err(SemanticError::Safety(SafetyError::UnboundParam { name, .. })) => {
                assert_eq!(name, "tenant_id");
            }
            other => panic!("expected UnboundParam, got {other:?}"),
        }
    }

    #[test]
    fn malicious_param_cannot_break_out() {
        let mut q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            ..Default::default()
        };
        q.params.insert("tenant_id".into(), "x' OR '1'='1".into());
        let sql = rls_catalog().compile_sql(&q).unwrap();
        // The value is one escaped literal, not a SQL fragment.
        assert!(sql.contains("tenant_id = 'x'' OR ''1''=''1'"), "{sql}");
    }

    // ---- safety guard rails ----

    #[test]
    fn require_at_least_one_filter_enforced() {
        let mut orders = orders_model();
        orders.safety = Some(SafetyPolicy {
            require_at_least_one_filter: true,
            ..Default::default()
        });
        let cat = SemanticCatalog::new(vec![orders]);
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            ..Default::default()
        };
        assert!(matches!(
            cat.compile_sql(&q),
            Err(SemanticError::Safety(SafetyError::NoFilters { .. }))
        ));
    }

    #[test]
    fn max_rows_clamps_limit() {
        let mut orders = orders_model();
        orders.safety = Some(SafetyPolicy {
            max_rows: Some(100),
            ..Default::default()
        });
        let cat = SemanticCatalog::new(vec![orders]);
        // Requested 5000 → clamped to 100.
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            limit: Some(5000),
            ..Default::default()
        };
        assert!(cat.compile_sql(&q).unwrap().contains("LIMIT 100"));
        // Unbounded → cap becomes the default limit.
        let q2 = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            ..Default::default()
        };
        assert!(cat.compile_sql(&q2).unwrap().contains("LIMIT 100"));
    }

    // ---- fan-out & aggregate-locality ----

    /// An `order_items` fact (many per order) with a `qty` measure, plus the
    /// `orders → order_items` (`one_to_many`) relationship declared on orders.
    fn order_items_catalog() -> SemanticCatalog {
        let mut orders = orders_model();
        orders.relationships = vec![Relationship {
            name: "items".into(),
            kind: RelationshipKind::OneToMany,
            target_model: "order_items".into(),
            join_predicate: "this.id = order_items.order_id".into(),
        }];
        let order_items = SemanticModel {
            name: "order_items".into(),
            description: None,
            source: "shop.order_items".into(),
            primary_key: vec!["id".into()],
            dimensions: vec![Dimension {
                name: "sku".into(),
                expr: "sku".into(),
                data_type: DimensionType::String,
                time_grains: vec![],
                description: None,
            }],
            measures: vec![Measure {
                name: "qty".into(),
                agg: MeasureAgg::Sum,
                expr: "quantity".into(),
                filters: vec![],
                format: None,
                description: None,
            }],
            relationships: vec![],
            pre_aggregations: vec![],
            segments: vec![],
            safety: None,
        };
        SemanticCatalog::new(vec![orders, order_items])
    }

    #[test]
    fn two_facts_compile_to_aggregate_locality() {
        // The canonical aggregate-locality case: revenue (one-per-order) and qty
        // (many-per-order) grouped by a shared dimension must each aggregate at
        // their own grain in a CTE, then FULL OUTER JOIN on the shared key.
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into(), "order_items.qty".into()],
            dimensions: vec!["orders.status".into()],
            ..Default::default()
        };
        let sql = order_items_catalog().compile_sql(&q).unwrap();
        assert!(sql.starts_with("WITH "), "{sql}");
        // orders aggregates alone; order_items joins back to orders for the dim.
        assert!(
            sql.contains("\"_orders\" AS (") && sql.contains("\"_order_items\" AS ("),
            "{sql}"
        );
        assert!(
            sql.contains("SUM(orders.total_amount) AS \"orders.revenue\""),
            "{sql}"
        );
        assert!(
            sql.contains("SUM(order_items.quantity) AS \"order_items.qty\""),
            "{sql}"
        );
        assert!(
            sql.contains("INNER JOIN \"shop\".\"orders\" AS orders"),
            "join order_items back to orders for the shared dim:\n{sql}"
        );
        assert!(
            sql.contains(
                "FULL OUTER JOIN \"_order_items\" ON COALESCE(\"_orders\".\"orders.status\") \
                 IS NOT DISTINCT FROM \"_order_items\".\"orders.status\""
            ),
            "{sql}"
        );
        assert!(
            sql.contains(
                "COALESCE(\"_orders\".\"orders.status\", \"_order_items\".\"orders.status\") \
                 AS \"orders.status\""
            ),
            "{sql}"
        );
    }

    #[test]
    fn multi_fact_grouped_by_many_side_fans_out() {
        // Grouping by `order_items.sku` fans out the one-per-order revenue.
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into(), "order_items.qty".into()],
            dimensions: vec!["order_items.sku".into()],
            ..Default::default()
        };
        match order_items_catalog().compile_sql(&q) {
            Err(SemanticError::FanOut { measure, via }) => {
                assert_eq!(measure, "orders.revenue");
                assert_eq!(via, "order_items");
            }
            other => panic!("expected FanOut, got {other:?}"),
        }
    }

    #[test]
    fn multi_fact_cte_applies_joined_models_rls() {
        // orders carries RLS. In aggregate-locality form the order_items CTE
        // joins back to orders, so it must AND-in orders' tenant predicate too —
        // otherwise its aggregate would span every tenant.
        let mut orders = orders_model();
        orders.relationships = vec![Relationship {
            name: "items".into(),
            kind: RelationshipKind::OneToMany,
            target_model: "order_items".into(),
            join_predicate: "this.id = order_items.order_id".into(),
        }];
        orders.safety = Some(SafetyPolicy {
            required_predicates: vec!["tenant_id = ${param:tenant_id}".into()],
            ..Default::default()
        });
        let order_items = SemanticModel {
            name: "order_items".into(),
            description: None,
            source: "shop.order_items".into(),
            primary_key: vec!["order_id".into(), "sku".into()],
            dimensions: vec![Dimension {
                name: "sku".into(),
                expr: "sku".into(),
                data_type: DimensionType::String,
                time_grains: vec![],
                description: None,
            }],
            measures: vec![Measure {
                name: "qty".into(),
                agg: MeasureAgg::Sum,
                expr: "quantity".into(),
                filters: vec![],
                format: None,
                description: None,
            }],
            relationships: vec![],
            pre_aggregations: vec![],
            segments: vec![],
            safety: None,
        };
        let cat = SemanticCatalog::new(vec![orders, order_items]);
        let mut q = SemanticQuery {
            measures: vec!["orders.revenue".into(), "order_items.qty".into()],
            dimensions: vec!["orders.status".into()],
            ..Default::default()
        };
        q.params.insert("tenant_id".into(), "acme".into());
        let sql = cat.compile_sql(&q).unwrap();
        // The predicate must appear twice: once per CTE (orders' own + the
        // order_items CTE that joins orders).
        assert_eq!(
            sql.matches("tenant_id = 'acme'").count(),
            2,
            "RLS must apply in every CTE that touches orders:\n{sql}"
        );
    }

    #[test]
    fn multi_fact_disconnected_dimension_errors() {
        // customers is unrelated to orders, so the customers CTE cannot produce
        // the orders.status grouping key.
        let mut customers = customer_model();
        customers.measures = vec![Measure {
            name: "customer_count".into(),
            agg: MeasureAgg::CountDistinct,
            expr: "id".into(),
            filters: vec![],
            format: None,
            description: None,
        }];
        let cat = SemanticCatalog::new(vec![orders_model(), customers]);
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into(), "customers.customer_count".into()],
            dimensions: vec!["orders.status".into()],
            ..Default::default()
        };
        assert!(
            matches!(
                cat.compile_sql(&q),
                Err(SemanticError::DisconnectedMember { .. })
            ),
            "{:?}",
            cat.compile_sql(&q)
        );
    }

    #[test]
    fn ambiguous_join_path_errors() {
        // Diamond: a → b → d and a → c → d are two equal-length paths to d.
        let mk = |name: &str, rels: Vec<Relationship>| SemanticModel {
            name: name.into(),
            description: None,
            source: format!("shop.{name}"),
            primary_key: vec!["id".into()],
            dimensions: vec![Dimension {
                name: "label".into(),
                expr: "label".into(),
                data_type: DimensionType::String,
                time_grains: vec![],
                description: None,
            }],
            measures: vec![Measure {
                name: "n".into(),
                agg: MeasureAgg::Count,
                expr: "id".into(),
                filters: vec![],
                format: None,
                description: None,
            }],
            relationships: rels,
            pre_aggregations: vec![],
            segments: vec![],
            safety: None,
        };
        let rel = |name: &str, target: &str| Relationship {
            name: name.into(),
            kind: RelationshipKind::ManyToOne,
            target_model: target.into(),
            join_predicate: format!("this.{target}_id = {target}.id"),
        };
        let cat = SemanticCatalog::new(vec![
            mk("a", vec![rel("to_b", "b"), rel("to_c", "c")]),
            mk("b", vec![rel("to_d", "d")]),
            mk("c", vec![rel("to_d", "d")]),
            mk("d", vec![]),
        ]);
        let q = SemanticQuery {
            measures: vec!["a.n".into()],
            dimensions: vec!["d.label".into()],
            ..Default::default()
        };
        assert!(
            matches!(
                cat.compile_sql(&q),
                Err(SemanticError::AmbiguousJoinPath { .. })
            ),
            "{:?}",
            cat.compile_sql(&q)
        );
    }

    // ---- WHERE vs HAVING classification ----

    #[test]
    fn measure_filter_becomes_having() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status".into()],
            filters: vec![SemanticFilter {
                member: "orders.revenue".into(),
                op: FilterOp::Gt,
                values: vec!["1000".into()],
            }],
            ..Default::default()
        };
        let sql = catalog().compile_sql(&q).unwrap();
        assert!(sql.contains("HAVING SUM(total_amount) > 1000"), "{sql}");
        assert!(
            !sql.contains("WHERE"),
            "measure filter must not be WHERE:\n{sql}"
        );
    }

    #[test]
    fn dimension_filter_stays_where() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status".into()],
            filters: vec![SemanticFilter {
                member: "orders.status".into(),
                op: FilterOp::Equals,
                values: vec!["paid".into()],
            }],
            ..Default::default()
        };
        let sql = catalog().compile_sql(&q).unwrap();
        assert!(sql.contains("WHERE status = 'paid'"), "{sql}");
        assert!(!sql.contains("HAVING"), "{sql}");
    }

    #[test]
    fn multi_fact_measure_filter_is_outer() {
        // A measure threshold over a multi-fact query filters the joined result.
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into(), "order_items.qty".into()],
            dimensions: vec!["orders.status".into()],
            filters: vec![SemanticFilter {
                member: "orders.revenue".into(),
                op: FilterOp::Gt,
                values: vec!["1000".into()],
            }],
            ..Default::default()
        };
        let sql = order_items_catalog().compile_sql(&q).unwrap();
        // The outer filter references the owning CTE column, not the output
        // alias (a `WHERE` cannot see `SELECT` aliases).
        assert!(
            sql.contains("\nWHERE \"_orders\".\"orders.revenue\" > 1000"),
            "{sql}"
        );
    }

    // ---- segments ----

    fn segment_catalog() -> SemanticCatalog {
        let mut orders = orders_model();
        orders.segments = vec![pawrly_core::semantic::Segment {
            name: "high_value".into(),
            description: None,
            filters: vec![SemanticFilter {
                member: "orders.status".into(),
                op: FilterOp::Equals,
                values: vec!["paid".into()],
            }],
        }];
        SemanticCatalog::new(vec![orders])
    }

    #[test]
    fn segment_expands_into_filters() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status".into()],
            segments: vec!["orders.high_value".into()],
            ..Default::default()
        };
        let sql = segment_catalog().compile_sql(&q).unwrap();
        assert!(sql.contains("WHERE status = 'paid'"), "{sql}");
    }

    #[test]
    fn unknown_segment_errors() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            segments: vec!["orders.nope".into()],
            ..Default::default()
        };
        assert!(matches!(
            segment_catalog().compile_sql(&q),
            Err(SemanticError::UnknownSegment(_))
        ));
    }

    // ---- pre-aggregation materialization & rollup rewrite ----

    fn preagg(name: &str, dims: &[&str], measures: &[&str]) -> PreAggregation {
        PreAggregation {
            name: name.into(),
            dimensions: dims.iter().map(|s| (*s).into()).collect(),
            measures: measures.iter().map(|s| (*s).into()).collect(),
            refresh: None,
            partition_by: None,
        }
    }

    fn preagg_orders_catalog() -> SemanticCatalog {
        let mut orders = orders_model();
        orders.pre_aggregations =
            vec![preagg("daily", &["order_date.day", "status"], &["revenue"])];
        SemanticCatalog::new(vec![orders])
    }

    #[test]
    fn compiles_preagg_materialization() {
        let sql = preagg_orders_catalog()
            .compile_preagg_sql("orders", "daily")
            .unwrap();
        assert!(
            sql.contains("DATE_TRUNC('day', ordered_at) AS \"order_date\""),
            "{sql}"
        );
        assert!(sql.contains("status AS \"status\""), "{sql}");
        assert!(sql.contains("SUM(total_amount) AS \"revenue\""), "{sql}");
        assert!(sql.contains("FROM \"shop\".\"orders\""), "{sql}");
        assert!(
            sql.contains("GROUP BY DATE_TRUNC('day', ordered_at), status"),
            "{sql}"
        );
    }

    #[test]
    fn candidate_rollup_matches_additive_query() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status".into()],
            ..Default::default()
        };
        let r = preagg_orders_catalog()
            .candidate_rollup(&q)
            .expect("rollup");
        assert_eq!(r.model, "orders");
        assert_eq!(r.preagg, "daily");
        assert_eq!(r.schema(), "semantic");
        assert_eq!(r.table(), "orders__daily");
    }

    #[test]
    fn compiles_against_rollup_with_reaggregation() {
        // A coarser grain than the rollup (day → month): re-truncate + re-sum.
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.order_date.month".into()],
            ..Default::default()
        };
        let cat = preagg_orders_catalog();
        let r = cat.candidate_rollup(&q).unwrap();
        let sql = cat.compile_rollup_sql(&q, &r).unwrap();
        assert!(
            sql.contains("FROM \"semantic\".\"orders__daily\" AS orders"),
            "{sql}"
        );
        assert!(
            sql.contains("SUM(orders.\"revenue\") AS \"orders.revenue\""),
            "{sql}"
        );
        assert!(
            sql.contains("DATE_TRUNC('month', orders.\"order_date\")"),
            "{sql}"
        );
    }

    #[test]
    fn rollup_skipped_for_nonadditive_measure() {
        let mut orders = orders_model();
        orders.measures.push(Measure {
            name: "order_count".into(),
            agg: MeasureAgg::CountDistinct,
            expr: "id".into(),
            filters: vec![],
            format: None,
            description: None,
        });
        orders.pre_aggregations = vec![preagg("by_status", &["status"], &["order_count"])];
        let cat = SemanticCatalog::new(vec![orders]);
        let q = SemanticQuery {
            measures: vec!["orders.order_count".into()],
            dimensions: vec!["orders.status".into()],
            ..Default::default()
        };
        assert!(cat.candidate_rollup(&q).is_none());
    }

    #[test]
    fn rollup_skipped_for_rls_model() {
        let mut orders = orders_model();
        orders.safety = Some(SafetyPolicy {
            required_predicates: vec!["tenant_id = ${param:tenant_id}".into()],
            ..Default::default()
        });
        orders.pre_aggregations = vec![preagg("by_status", &["status"], &["revenue"])];
        let cat = SemanticCatalog::new(vec![orders]);
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status".into()],
            ..Default::default()
        };
        assert!(cat.candidate_rollup(&q).is_none());
    }

    #[test]
    fn rollup_skipped_with_time_zone() {
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.status".into()],
            time_zone: Some("America/New_York".into()),
            ..Default::default()
        };
        assert!(preagg_orders_catalog().candidate_rollup(&q).is_none());
    }

    #[test]
    fn rollup_skipped_when_dimension_uncovered() {
        // country is not in the rollup, so it cannot serve this query.
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["orders.country".into()],
            ..Default::default()
        };
        assert!(preagg_orders_catalog().candidate_rollup(&q).is_none());
    }
}
