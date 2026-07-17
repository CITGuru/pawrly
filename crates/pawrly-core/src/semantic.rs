//! Semantic-layer types: business-named models, dimensions, measures, and the
//! structured `SemanticQuery` clients submit against them.
//!
//! These are pure data types with no engine dependency. The compiler that
//! turns a [`SemanticQuery`] into executable SQL/`LogicalPlan` lives in the
//! `pawrly-semantic` crate; the three [`crate::EngineService`] methods that
//! expose models over every transport take and return the types defined here.
//!
//! Field naming matches the documented YAML surface (`source`, `type`,
//! `grains`) so a `semantic:` config block deserializes straight into these
//! structs.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::safety::SafetyPolicy;

/// A logical entity anchored on one physical table.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SemanticModel {
    /// Business name, e.g. `orders`. Used as the member prefix.
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// `"schema.table"` reference to a table registered in the `pawrly`
    /// catalog (schema = source name). Parsed by the compiler, not at
    /// deserialization time, so a malformed value surfaces as a query-time
    /// error rather than a config-load panic.
    pub source: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primary_key: Vec<String>,

    pub dimensions: Vec<Dimension>,

    pub measures: Vec<Measure>,

    /// Joins to other models, walked by the compiler to answer queries whose
    /// members span models.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<Relationship>,

    /// Declared rollups. The rollup matcher rewrites the `FROM` clause to a
    /// materialized pre-agg when one covers the query; materialization itself
    /// is handled by the cache layer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_aggregations: Vec<PreAggregation>,

    /// Named, reusable filter sets. A query references one as `model.segment`
    /// and the compiler expands it into its predicates before planning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<Segment>,

    /// Per-model guard rails. `required_predicates` (RLS) are AND-ed into every
    /// compiled query for this model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<SafetyPolicy>,
}

/// A named, reusable set of filters defined on a [`SemanticModel`].
///
/// A segment is a label that expands into its predicates at compile time,
/// referenced from a [`SemanticQuery`] as `model.segment` (e.g.
/// `"orders.high_value"`). `describe_semantic_model` returns the model's
/// segments so a client can discover and compose them, and they are auditable
/// because the predicates live in trusted config — never in the request.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Segment {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Predicates AND-ed into the query when the segment is selected.
    pub filters: Vec<SemanticFilter>,
}

/// A join from one model to another. `join_predicate` is a raw SQL fragment in
/// which `this` aliases the declaring model and the target is referenced by its
/// model name (e.g. `"this.customer_id = customers.id"`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Relationship {
    pub name: String,
    pub kind: RelationshipKind,
    /// Name of the model this relationship joins to.
    #[serde(rename = "target")]
    pub target_model: String,
    /// SQL join condition; `this` = the declaring model's alias.
    #[serde(rename = "on")]
    pub join_predicate: String,
}

/// Cardinality of a [`Relationship`], which picks the join type: `*-to-one`
/// joins are `INNER`, `*-to-many` are `LEFT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipKind {
    ManyToOne,
    OneToMany,
    OneToOne,
}

impl RelationshipKind {
    /// SQL join keyword for this cardinality.
    #[must_use]
    pub fn join_kind(self) -> &'static str {
        match self {
            // *-to-many can drop rows from the parent; keep them with LEFT.
            Self::OneToMany => "LEFT",
            Self::ManyToOne | Self::OneToOne => "INNER",
        }
    }
}

/// A declared rollup: a pre-materialized grouping that the matcher can swap in
/// for the base table when it covers the query.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PreAggregation {
    pub name: String,
    /// Dimension members, possibly grained, e.g. `["order_date.day", "status"]`.
    pub dimensions: Vec<String>,
    /// Measure names defined on the owning model.
    pub measures: Vec<String>,
    /// Refresh cadence; how often the materializer rebuilds the rollup.
    #[serde(
        default,
        with = "humantime_serde::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub refresh: Option<std::time::Duration>,
    /// Optional partition member, e.g. `"order_date.month"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_by: Option<String>,
}

/// A groupable attribute. `expr` is a raw SQL fragment over the base table.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Dimension {
    pub name: String,
    pub expr: String,
    #[serde(rename = "type")]
    pub data_type: DimensionType,
    /// Valid time grains; meaningful only when `data_type == Time`.
    #[serde(rename = "grains", default, skip_serializing_if = "Vec::is_empty")]
    pub time_grains: Vec<TimeGrain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The kind of a dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DimensionType {
    String,
    Number,
    Time,
    Bool,
}

/// A time-bucketing grain applied via `DATE_TRUNC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TimeGrain {
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl TimeGrain {
    /// Canonical lowercase token, also the `DATE_TRUNC` field argument.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Quarter => "quarter",
            Self::Year => "year",
        }
    }

    /// Parse from the canonical lowercase token.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "hour" => Self::Hour,
            "day" => Self::Day,
            "week" => Self::Week,
            "month" => Self::Month,
            "quarter" => Self::Quarter,
            "year" => Self::Year,
            _ => return None,
        })
    }

    /// Rank from finest (`hour` = 0) to coarsest (`year` = 5). Used by the
    /// rollup matcher: a pre-agg at grain G can satisfy a query at grain Q only
    /// when `G.rank() <= Q.rank()` (the rollup is at least as fine), since a
    /// coarser bucket is a one-step `DATE_TRUNC` away but a finer one is lost.
    ///
    /// Note `week` and `month` are not nested (a week spans two months), so
    /// neither is treated as coarser than the other; only equal-or-finer-by-rank
    /// compatibility is asserted here and the matcher additionally requires the
    /// grains be roll-up compatible via [`Self::can_roll_up_to`].
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::Hour => 0,
            Self::Day => 1,
            Self::Week => 2,
            Self::Month => 3,
            Self::Quarter => 4,
            Self::Year => 5,
        }
    }

    /// True when a value bucketed at `self` can be re-bucketed to `coarser`
    /// purely by `DATE_TRUNC` (no access to finer data). `day` rolls up to
    /// week/month/quarter/year; `month` to quarter/year; `week` only to itself
    /// (weeks don't nest cleanly into months/quarters/years).
    #[must_use]
    pub fn can_roll_up_to(self, coarser: Self) -> bool {
        if self == coarser {
            return true;
        }
        match self {
            Self::Hour => true,
            Self::Day => matches!(
                coarser,
                Self::Week | Self::Month | Self::Quarter | Self::Year
            ),
            Self::Week => false,
            Self::Month => matches!(coarser, Self::Quarter | Self::Year),
            Self::Quarter => matches!(coarser, Self::Year),
            Self::Year => false,
        }
    }
}

/// An aggregation formula.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Measure {
    pub name: String,
    pub agg: MeasureAgg,
    /// Raw SQL fragment the aggregate is applied to.
    pub expr: String,
    /// Measure-scoped predicates compiled into a `FILTER (WHERE ...)` clause.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<String>,
    /// Display format hint for clients (e.g. `"$#,##0.00"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The aggregation kind for a [`Measure`].
///
/// Externally tagged: unit variants deserialize from a bare string
/// (`agg: count_distinct`), and `Custom` from a map (`agg: { custom: { sql:
/// "..." } }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeasureAgg {
    Sum,
    Count,
    CountDistinct,
    Avg,
    Min,
    Max,
    Custom { sql: String },
}

impl MeasureAgg {
    /// Stable lowercase label for wire/display use.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Count => "count",
            Self::CountDistinct => "count_distinct",
            Self::Avg => "avg",
            Self::Min => "min",
            Self::Max => "max",
            Self::Custom { .. } => "custom",
        }
    }
}

/// A queryable business metric defined over one or more measures. Lives at the
/// workspace level (not nested in a model) so it can compose measures across
/// models. Evaluated *after* aggregation, as a projection over measure columns.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    /// Bare name; the member used to request it, e.g. `aov`. Must contain no
    /// `.` (reserved for `model.member`) and must not collide with a model name.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: MetricKind,
    /// Governed predicate AND-ed into every leaf measure of this metric (pushed
    /// to each leaf's `FILTER (WHERE …)`). Part of the metric's identity, unlike
    /// a caller's per-query `SemanticQuery.filters`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Display format hint, e.g. `"$#,##0.00"`, `"0.0%"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

/// Externally tagged: `kind: { ratio: { … } }`. Deserialized via
/// [`MetricKindRepr`] because `serde_yaml` only reads externally-tagged struct
/// variants from `!tag` syntax, and the map form is the documented surface.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", try_from = "MetricKindRepr")]
pub enum MetricKind {
    /// `numerator / NULLIF(denominator, 0)`. Each operand is a measure
    /// (`orders.revenue`) or metric, with an optional per-side filter.
    Ratio {
        numerator: Operand,
        denominator: Operand,
    },

    /// Scalar arithmetic over `{member}` references resolved from the aggregated
    /// output, e.g. `"({orders.revenue} - {orders.cost}) / NULLIF({orders.revenue}, 0)"`.
    /// A `{member | predicate}` token applies a per-token leaf filter.
    Derived { expr: String },

    /// Window aggregate over `measure` along the query's time grain, computed
    /// over the dense time spine. `agg` selects the window function.
    Cumulative {
        measure: String,
        #[serde(default)]
        window: CumulativeWindow,
        #[serde(default)]
        agg: WindowAgg,
    },

    /// Period-over-period: compare `measure` to itself `periods` grains back,
    /// aligned on the time spine so gaps don't misalign. `output` selects the
    /// prior value, the difference, or the growth ratio.
    Offset {
        measure: String,
        #[serde(default = "one")]
        periods: u32,
        #[serde(default)]
        output: OffsetOutput,
    },

    /// Part-of-whole: `measure` divided by a window aggregate of the same
    /// measure over `over` (a subset of the query's dimensions; empty = grand
    /// total). `agg` selects the denominator's window function.
    Share {
        measure: String,
        #[serde(default)]
        over: Vec<String>,
        #[serde(default)]
        agg: WindowAgg,
    },
}

fn one() -> u32 {
    1
}

impl MetricKind {
    /// Stable lowercase label for wire/display use.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ratio { .. } => "ratio",
            Self::Derived { .. } => "derived",
            Self::Cumulative { .. } => "cumulative",
            Self::Offset { .. } => "offset",
            Self::Share { .. } => "share",
        }
    }
}

impl Metric {
    /// Every member this metric references — measures (`model.measure`) or
    /// other metrics (dot-free) — with any per-operand / per-token filter.
    /// `Err` carries a `Derived` expression parse failure.
    pub fn references(&self) -> Result<Vec<(String, Option<String>)>, String> {
        match &self.kind {
            MetricKind::Ratio {
                numerator,
                denominator,
            } => Ok(vec![
                (numerator.member.clone(), numerator.filter.clone()),
                (denominator.member.clone(), denominator.filter.clone()),
            ]),
            MetricKind::Derived { expr } => Ok(derived_tokens(expr)?
                .into_iter()
                .map(|t| (t.member, t.filter))
                .collect()),
            MetricKind::Cumulative { measure, .. }
            | MetricKind::Offset { measure, .. }
            | MetricKind::Share { measure, .. } => Ok(vec![(measure.clone(), None)]),
        }
    }
}

/// One `{member}` / `{member | filter}` reference inside a `Derived` expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedToken {
    pub member: String,
    pub filter: Option<String>,
}

/// Extract the `{…}` tokens from a `Derived` expression, splitting each on the
/// first `|` into member and optional per-token filter. Rejects unbalanced
/// braces, empty tokens, and an expression with no token at all.
pub fn derived_tokens(expr: &str) -> Result<Vec<DerivedToken>, String> {
    let mut tokens = Vec::new();
    let mut rest = expr;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("unbalanced `{{` in derived expr `{expr}`"))?;
        let body = &after[..end];
        let (member, filter) = match body.split_once('|') {
            Some((m, f)) => (
                m.trim(),
                Some(f.trim().to_string()).filter(|s| !s.is_empty()),
            ),
            None => (body.trim(), None),
        };
        if member.is_empty() {
            return Err(format!("empty `{{}}` reference in derived expr `{expr}`"));
        }
        tokens.push(DerivedToken {
            member: member.to_string(),
            filter,
        });
        rest = &after[end + 1..];
    }
    if rest.contains('}') {
        return Err(format!("unbalanced `}}` in derived expr `{expr}`"));
    }
    if tokens.is_empty() {
        return Err(format!(
            "derived expr `{expr}` references no `{{member}}` token"
        ));
    }
    Ok(tokens)
}

/// The `kind:` map form, one key per metric kind.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MetricKindRepr {
    #[serde(default)]
    ratio: Option<RatioRepr>,
    #[serde(default)]
    derived: Option<DerivedRepr>,
    #[serde(default)]
    cumulative: Option<CumulativeRepr>,
    #[serde(default)]
    offset: Option<OffsetRepr>,
    #[serde(default)]
    share: Option<ShareRepr>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RatioRepr {
    numerator: Operand,
    denominator: Operand,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DerivedRepr {
    expr: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CumulativeRepr {
    measure: String,
    #[serde(default)]
    window: CumulativeWindow,
    #[serde(default)]
    agg: WindowAgg,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OffsetRepr {
    measure: String,
    #[serde(default = "one")]
    periods: u32,
    #[serde(default)]
    output: OffsetOutput,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShareRepr {
    measure: String,
    #[serde(default)]
    over: Vec<String>,
    #[serde(default)]
    agg: WindowAgg,
}

impl TryFrom<MetricKindRepr> for MetricKind {
    type Error = String;

    fn try_from(r: MetricKindRepr) -> Result<Self, String> {
        let mut kinds: Vec<MetricKind> = Vec::new();
        if let Some(k) = r.ratio {
            kinds.push(Self::Ratio {
                numerator: k.numerator,
                denominator: k.denominator,
            });
        }
        if let Some(k) = r.derived {
            kinds.push(Self::Derived { expr: k.expr });
        }
        if let Some(k) = r.cumulative {
            kinds.push(Self::Cumulative {
                measure: k.measure,
                window: k.window,
                agg: k.agg,
            });
        }
        if let Some(k) = r.offset {
            kinds.push(Self::Offset {
                measure: k.measure,
                periods: k.periods,
                output: k.output,
            });
        }
        if let Some(k) = r.share {
            kinds.push(Self::Share {
                measure: k.measure,
                over: k.over,
                agg: k.agg,
            });
        }
        let mut kinds = kinds.into_iter();
        match (kinds.next(), kinds.next()) {
            (Some(kind), None) => Ok(kind),
            _ => Err(
                "`kind:` must set exactly one of `ratio`, `derived`, `cumulative`, `offset`, \
                 or `share`"
                    .to_string(),
            ),
        }
    }
}

/// A `Ratio` operand: a measure/metric member with an optional governed filter.
/// Deserializes from a bare string (`orders.revenue`, no filter) or a map
/// (`{ member: orders.revenue, filter: "is_food" }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "OperandRepr")]
pub struct Operand {
    pub member: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

/// The two YAML spellings of an [`Operand`].
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
enum OperandRepr {
    Member(String),
    Full {
        member: String,
        #[serde(default)]
        filter: Option<String>,
    },
}

impl From<OperandRepr> for Operand {
    fn from(r: OperandRepr) -> Self {
        match r {
            OperandRepr::Member(member) => Self {
                member,
                filter: None,
            },
            OperandRepr::Full { member, filter } => Self { member, filter },
        }
    }
}

impl JsonSchema for Operand {
    fn schema_name() -> String {
        "Operand".to_string()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        OperandRepr::json_schema(generator)
    }
}

/// The window function used by `Cumulative` and `Share`. (`Count`/distinct are
/// deliberately excluded — re-aggregating per-period counts double-counts; use
/// a measure-tier additive count instead.)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowAgg {
    #[default]
    Sum,
    Avg,
    Min,
    Max,
}

/// `RunningTotal` = unbounded from the series start (never resets).
/// `GrainToDate { grain }` = resets at each calendar boundary (MTD/QTD/YTD).
/// `Trailing { periods }` = rolling window of `periods` spine rows.
///
/// Deliberately NOT named `ToDate` — MetricFlow/Cube `to_date` means
/// *period-to-date (resets)*, which is our `GrainToDate`, the opposite of a
/// running total; the name is avoided to prevent a silent semantic inversion.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", try_from = "CumulativeWindowRepr")]
pub enum CumulativeWindow {
    #[default]
    RunningTotal,
    GrainToDate {
        grain: TimeGrain,
    },
    Trailing {
        periods: u32,
    },
}

/// The two YAML spellings of a window: bare `running_total`, or a single-key
/// map (`{ grain_to_date: { grain: year } }` / `{ trailing: { periods: 7 } }`).
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
enum CumulativeWindowRepr {
    Name(String),
    Map {
        #[serde(default)]
        grain_to_date: Option<GrainToDateRepr>,
        #[serde(default)]
        trailing: Option<TrailingRepr>,
    },
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GrainToDateRepr {
    grain: TimeGrain,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TrailingRepr {
    periods: u32,
}

impl TryFrom<CumulativeWindowRepr> for CumulativeWindow {
    type Error = String;

    fn try_from(r: CumulativeWindowRepr) -> Result<Self, String> {
        match r {
            CumulativeWindowRepr::Name(s) if s == "running_total" => Ok(Self::RunningTotal),
            CumulativeWindowRepr::Name(s) => Err(format!(
                "unknown window `{s}` (expected `running_total`, `grain_to_date`, or `trailing`)"
            )),
            CumulativeWindowRepr::Map {
                grain_to_date: Some(g),
                trailing: None,
            } => Ok(Self::GrainToDate { grain: g.grain }),
            CumulativeWindowRepr::Map {
                grain_to_date: None,
                trailing: Some(t),
            } => Ok(Self::Trailing { periods: t.periods }),
            CumulativeWindowRepr::Map { .. } => {
                Err("`window:` must set exactly one of `grain_to_date` or `trailing`".to_string())
            }
        }
    }
}

/// What an `Offset` metric projects: the prior value, the difference from it,
/// or the growth ratio.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OffsetOutput {
    #[default]
    Value,
    Delta,
    Growth,
}

/// A declared calendar table for window metrics (`semantic.time_spine:`).
/// Absent, the compiler generates a dense date axis at the query grain.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimeSpine {
    /// `<source>.<table>` of the calendar table.
    pub source: String,
    /// Its date/timestamp column.
    pub column: String,
}

/// A structured question against the semantic layer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SemanticQuery {
    /// Measure members, e.g. `["orders.revenue"]`.
    #[serde(default)]
    pub measures: Vec<String>,
    /// Dimension members, e.g. `["orders.order_date.month", "orders.status"]`.
    #[serde(default)]
    pub dimensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<SemanticFilter>,
    /// Named segments to apply, each `model.segment`. Their predicates are
    /// AND-ed in alongside `filters` at compile time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order_by: Vec<SemanticOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// Values bound to `${param:NAME}` placeholders in a model's
    /// `required_predicates` (RLS). Bound as escaped SQL literals at compile
    /// time — never interpolated as SQL fragments.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub params: HashMap<String, String>,
}

/// A predicate over one member.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SemanticFilter {
    /// `"orders.status"` or `"orders.order_date.month"`.
    pub member: String,
    pub op: FilterOp,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

/// Comparison operators usable in a [`SemanticFilter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Equals,
    NotEquals,
    In,
    NotIn,
    Gt,
    Gte,
    Lt,
    Lte,
    /// `values: [start, end]` → `BETWEEN start AND end`.
    InRange,
    Contains,
    StartsWith,
    EndsWith,
    IsNull,
    IsNotNull,
}

/// An ordering term.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SemanticOrder {
    pub member: String,
    #[serde(default)]
    pub direction: OrderDir,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OrderDir {
    #[default]
    Asc,
    Desc,
}

/// Lightweight list-row form returned by `list_semantic_models`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SemanticModelInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: String,
    pub dimension_count: u32,
    pub measure_count: u32,
}

/// Full model spec returned by `describe_semantic_model` for grounding.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SemanticModelDescription {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primary_key: Vec<String>,
    pub dimensions: Vec<Dimension>,
    pub measures: Vec<Measure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<Relationship>,
    /// The model's named, reusable filter sets, so a client can discover and
    /// apply them by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<Segment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measure_agg_unit_variant_from_bare_string() {
        let m: Measure =
            serde_yaml::from_str("name: order_count\nagg: count_distinct\nexpr: id\n").unwrap();
        assert_eq!(m.agg, MeasureAgg::CountDistinct);
        assert_eq!(m.agg.label(), "count_distinct");
    }

    #[test]
    fn measure_agg_custom_from_yaml_tag() {
        // serde_yaml encodes externally-tagged enum struct variants with a
        // `!variant` tag; unit variants (above) still read from a bare string.
        let m: Measure =
            serde_yaml::from_str("name: ratio\nagg: !custom { sql: \"SUM(a)/SUM(b)\" }\nexpr: a\n")
                .unwrap();
        assert_eq!(
            m.agg,
            MeasureAgg::Custom {
                sql: "SUM(a)/SUM(b)".into()
            }
        );
    }

    #[test]
    fn measure_agg_custom_round_trips_via_json() {
        // The JSON path (used outside YAML config) uses the standard
        // externally-tagged map form.
        let v = serde_json::json!({"name": "ratio", "agg": {"custom": {"sql": "x"}}, "expr": "a"});
        let m: Measure = serde_json::from_value(v).unwrap();
        assert_eq!(m.agg, MeasureAgg::Custom { sql: "x".into() });
    }

    #[test]
    fn dimension_uses_yaml_field_aliases() {
        let d: Dimension = serde_yaml::from_str(
            "name: order_date\nexpr: ordered_at\ntype: time\ngrains: [day, month]\n",
        )
        .unwrap();
        assert_eq!(d.data_type, DimensionType::Time);
        assert_eq!(d.time_grains, vec![TimeGrain::Day, TimeGrain::Month]);
    }

    #[test]
    fn metric_ratio_from_yaml_with_bare_string_operands() {
        let m: Metric = serde_yaml::from_str(
            "name: aov\nkind: { ratio: { numerator: orders.revenue, denominator: orders.order_count } }\n",
        )
        .unwrap();
        let MetricKind::Ratio {
            numerator,
            denominator,
        } = &m.kind
        else {
            panic!("expected ratio");
        };
        assert_eq!(numerator.member, "orders.revenue");
        assert!(numerator.filter.is_none());
        assert_eq!(denominator.member, "orders.order_count");
    }

    #[test]
    fn metric_operand_map_form_carries_filter() {
        let m: Metric = serde_yaml::from_str(
            "name: r\nkind:\n  ratio:\n    numerator: { member: s.cancels, filter: \"plan = 'x'\" }\n    denominator: s.count\n",
        )
        .unwrap();
        let MetricKind::Ratio { numerator, .. } = &m.kind else {
            panic!("expected ratio");
        };
        assert_eq!(numerator.filter.as_deref(), Some("plan = 'x'"));
    }

    #[test]
    fn metric_offset_defaults_periods_and_output() {
        let m: Metric =
            serde_yaml::from_str("name: mom\nkind: { offset: { measure: orders.revenue } }\n")
                .unwrap();
        let MetricKind::Offset {
            periods, output, ..
        } = m.kind
        else {
            panic!("expected offset");
        };
        assert_eq!(periods, 1);
        assert_eq!(output, OffsetOutput::Value);
    }

    #[test]
    fn metric_cumulative_window_forms() {
        let m: Metric = serde_yaml::from_str(
            "name: ytd\nkind: { cumulative: { measure: orders.revenue, window: { grain_to_date: { grain: year } }, agg: avg } }\n",
        )
        .unwrap();
        let MetricKind::Cumulative { window, agg, .. } = m.kind else {
            panic!("expected cumulative");
        };
        assert!(matches!(
            window,
            CumulativeWindow::GrainToDate {
                grain: TimeGrain::Year
            }
        ));
        assert_eq!(agg, WindowAgg::Avg);
    }
}
