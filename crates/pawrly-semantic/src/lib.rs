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

pub mod rollup;

use pawrly_core::semantic::{
    Dimension, DimensionType, FilterOp, Measure, MeasureAgg, OrderDir, SemanticFilter,
    SemanticModel, SemanticModelDescription, SemanticModelInfo, SemanticOrder, SemanticQuery,
    TimeGrain,
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

/// The set of semantic models defined in a workspace.
#[derive(Debug, Default, Clone)]
pub struct SemanticCatalog {
    models: HashMap<String, Arc<SemanticModel>>,
}

impl SemanticCatalog {
    /// Build a catalog from the configured models.
    #[must_use]
    pub fn new(models: Vec<SemanticModel>) -> Self {
        let models = models
            .into_iter()
            .map(|m| (m.name.clone(), Arc::new(m)))
            .collect();
        Self { models }
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
        })
    }

    /// Compile a query into a SQL string over the `pawrly` catalog.
    ///
    /// A single-model query emits the original unqualified, unaliased form
    /// (`FROM "schema"."table"`, `GROUP BY status`). A query whose members
    /// span related models switches to an aliased form — every base table gets
    /// `AS <model>` and bare-column expressions are qualified with that alias
    /// so joins are unambiguous. Complex (non-bare-identifier) expressions are
    /// passed through verbatim; in a joined model the author should qualify
    /// them, exactly as for raw `required_predicates`.
    pub fn compile_sql(&self, q: &SemanticQuery) -> Result<String, SemanticError> {
        if q.measures.is_empty() && q.dimensions.is_empty() {
            return Err(SemanticError::EmptyQuery);
        }

        let root = self.resolve_root(q)?;
        let referenced = self.referenced_models(q)?;
        let plan = self.build_join_plan(&root, &referenced)?;
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

        let from = self.render_from(&root, &plan, joined)?;

        // Safety guard rails on the user-supplied filters (root model).
        self.enforce_filter_safety(&root, q)?;

        let mut wheres: Vec<String> = Vec::new();
        for f in &q.filters {
            let model = self.model_for_member(&f.member)?;
            wheres.push(resolve_filter(
                model,
                f,
                alias_for(&model.name).as_deref(),
                tz,
            )?);
        }
        // Required predicates (RLS + always-on filters) for every model the
        // query touches, AND-ed in with params bound as escaped literals.
        for model_name in &referenced {
            // `referenced` is built only from models present in the catalog.
            let Some(model) = self.models.get(model_name) else {
                continue;
            };
            if let Some(safety) = &model.safety {
                for pred in &safety.required_predicates {
                    wheres.push(bind_params(pred, &q.params, model_name)?);
                }
            }
        }

        // ORDER BY — every term must reference a selected member.
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

        let limit = effective_limit(q.limit, self.max_rows(&root));

        let mut sql = format!("SELECT {}\nFROM {from}", select_items.join(", "));
        if !wheres.is_empty() {
            sql.push_str("\nWHERE ");
            sql.push_str(&wheres.join(" AND "));
        }
        if !dim_exprs.is_empty() {
            sql.push_str("\nGROUP BY ");
            sql.push_str(&dim_exprs.join(", "));
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

    /// Measures must all share one root model. With no measures, the first
    /// dimension's model anchors the query and relatives join in.
    fn resolve_root(&self, q: &SemanticQuery) -> Result<String, SemanticError> {
        let mut roots: Vec<String> = Vec::new();
        for member in &q.measures {
            let model = member_model(member)?.to_string();
            if !roots.contains(&model) {
                roots.push(model);
            }
        }
        match roots.len() {
            0 => match q.dimensions.first() {
                Some(member) => Ok(member_model(member)?.to_string()),
                None => Err(SemanticError::EmptyQuery),
            },
            1 => Ok(roots.remove(0)),
            _ => {
                roots.sort();
                Err(SemanticError::AmbiguousRoot(roots))
            }
        }
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

    /// BFS the relationship graph from `root`, emitting the joins needed to
    /// connect every referenced model. Parents are always emitted before
    /// children. Unknown targets and unreachable models are hard errors.
    fn build_join_plan(
        &self,
        root: &str,
        referenced: &[String],
    ) -> Result<JoinPlan, SemanticError> {
        use std::collections::{HashSet, VecDeque};

        let mut parent_from: HashMap<String, String> = HashMap::new();
        let mut edge: HashMap<String, JoinStep> = HashMap::new();
        let mut discovery: Vec<String> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(root.to_string());
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(root.to_string());

        while let Some(from) = queue.pop_front() {
            // Only catalog models are ever enqueued.
            let Some(model) = self.models.get(&from) else {
                continue;
            };
            for rel in &model.relationships {
                if !self.models.contains_key(&rel.target_model) {
                    return Err(SemanticError::UnknownRelationshipTarget {
                        model: from.clone(),
                        rel: rel.name.clone(),
                        target: rel.target_model.clone(),
                    });
                }
                if !visited.insert(rel.target_model.clone()) {
                    continue; // already reached; ignore back-edges (no cycles)
                }
                let target_table = self.qualified_table(&rel.target_model)?;
                // `this.` aliases the declaring model; the target is referred
                // to by its own model name (which is also its table alias).
                let on = rel.join_predicate.replace("this.", &format!("{from}."));
                edge.insert(
                    rel.target_model.clone(),
                    JoinStep {
                        target: rel.target_model.clone(),
                        table: target_table,
                        kind: rel.kind.join_kind(),
                        on,
                    },
                );
                parent_from.insert(rel.target_model.clone(), from.clone());
                discovery.push(rel.target_model.clone());
                queue.push_back(rel.target_model.clone());
            }
        }

        // Which models must actually be joined: every referenced non-root
        // model plus the intermediate hops on its path back to root.
        let mut needed: HashSet<String> = HashSet::new();
        for model in referenced {
            if model == root {
                continue;
            }
            if !visited.contains(model) {
                return Err(SemanticError::UnreachableModel {
                    root: root.to_string(),
                    model: model.clone(),
                });
            }
            let mut cur = model.clone();
            while cur != root {
                needed.insert(cur.clone());
                cur = parent_from
                    .get(&cur)
                    .cloned()
                    .ok_or_else(|| SemanticError::RelationshipCycle(cur.clone()))?;
            }
        }

        let joins = discovery
            .into_iter()
            .filter(|m| needed.contains(m))
            .filter_map(|m| edge.remove(&m))
            .collect();
        Ok(JoinPlan { joins })
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

    /// Render the `FROM` (+ `JOIN`) clause. Unaliased for single-model queries
    /// to preserve the original output; aliased (`AS <model>`) when joining.
    fn render_from(
        &self,
        root: &str,
        plan: &JoinPlan,
        joined: bool,
    ) -> Result<String, SemanticError> {
        let root_table = self.qualified_table(root)?;
        if !joined {
            return Ok(root_table);
        }
        let mut from = format!("{root_table} AS {root}");
        for step in &plan.joins {
            from.push_str(&format!(
                "\n{} JOIN {} AS {}\n  ON {}",
                step.kind, step.table, step.target, step.on
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
}

/// One emitted JOIN in a [`JoinPlan`].
struct JoinStep {
    /// Target model name, used as the table alias.
    target: String,
    /// `"schema"."table"` for the target.
    table: String,
    /// SQL join keyword (`INNER` / `LEFT`).
    kind: &'static str,
    /// Join condition with `this.` rewritten to the declaring model's alias.
    on: String,
}

/// The ordered joins connecting a query's referenced models to its root.
struct JoinPlan {
    joins: Vec<JoinStep>,
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

fn resolve_filter(
    model: &SemanticModel,
    f: &SemanticFilter,
    alias: Option<&str>,
    tz: Option<&str>,
) -> Result<String, SemanticError> {
    let expr = resolve_dimension_expr(model, &f.member, alias, tz)?;
    let one = |member: &str| -> Result<&str, SemanticError> {
        f.values
            .first()
            .map(String::as_str)
            .ok_or_else(|| SemanticError::Compile(format!("filter on `{member}` requires a value")))
    };
    let predicate = match f.op {
        FilterOp::Equals => format!("{expr} = {}", lit(one(&f.member)?)),
        FilterOp::NotEquals => format!("{expr} <> {}", lit(one(&f.member)?)),
        FilterOp::Gt => format!("{expr} > {}", lit(one(&f.member)?)),
        FilterOp::Gte => format!("{expr} >= {}", lit(one(&f.member)?)),
        FilterOp::Lt => format!("{expr} < {}", lit(one(&f.member)?)),
        FilterOp::Lte => format!("{expr} <= {}", lit(one(&f.member)?)),
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
                .map(|v| lit(v))
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
                (Some(lo), Some(hi)) => format!("{expr} BETWEEN {} AND {}", lit(lo), lit(hi)),
                _ => {
                    return Err(SemanticError::Compile(format!(
                        "in_range filter on `{}` requires two values",
                        f.member
                    )));
                }
            }
        }
        FilterOp::Contains => format!("{expr} LIKE {}", lit(&format!("%{}%", one(&f.member)?))),
        FilterOp::StartsWith => format!("{expr} LIKE {}", lit(&format!("{}%", one(&f.member)?))),
        FilterOp::EndsWith => format!("{expr} LIKE {}", lit(&format!("%{}", one(&f.member)?))),
        FilterOp::IsNull => format!("{expr} IS NULL"),
        FilterOp::IsNotNull => format!("{expr} IS NOT NULL"),
    };
    Ok(predicate)
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
        Dimension, DimensionType, Measure, MeasureAgg, Relationship, RelationshipKind, TimeGrain,
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
    fn ambiguous_root_errors() {
        // Measures drawn from two different models have no single root.
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
        assert!(matches!(
            cat.compile_sql(&q),
            Err(SemanticError::AmbiguousRoot(_))
        ));
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

    #[test]
    fn one_to_many_uses_left_join() {
        let mut orders = orders_model();
        orders.relationships = vec![Relationship {
            name: "customer".into(),
            kind: RelationshipKind::OneToMany,
            target_model: "customers".into(),
            join_predicate: "this.customer_id = customers.id".into(),
        }];
        let cat = SemanticCatalog::new(vec![orders, customer_model()]);
        let q = SemanticQuery {
            measures: vec!["orders.revenue".into()],
            dimensions: vec!["customers.region".into()],
            ..Default::default()
        };
        let sql = cat.compile_sql(&q).unwrap();
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
            Err(SemanticError::UnreachableModel { .. })
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
}
