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

    /// Per-model guard rails. `required_predicates` (RLS) are AND-ed into every
    /// compiled query for this model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<SafetyPolicy>,
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
}
