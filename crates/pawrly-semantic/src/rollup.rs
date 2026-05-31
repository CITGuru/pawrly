//! Rollup matching — pure logic, no IO.
//!
//! Given a [`SemanticQuery`] and a model's declared [`PreAggregation`]s, decide
//! whether some rollup *covers* the query: it groups by at least the query's
//! dimensions (at a compatible-or-finer grain), aggregates at least the query's
//! measures, and its dimensions span every filtered member. When one does, the
//! engine can read the (materialized) rollup instead of the live base table.
//!
//! This module decides **coverage only**. Whether a covering rollup is actually
//! materialized and *fresh* is the caller's call (it consults the cache
//! manifest). When nothing covers — or the covering rollup isn't fresh — the
//! query falls through to the live base table; a missing rollup never fails a
//! query. Materialization itself is handled by the cache layer; until a rollup
//! is registered there, [`match_rollup`] is consulted but never satisfied in
//! practice.

use pawrly_core::semantic::{PreAggregation, SemanticModel, SemanticQuery, TimeGrain};

/// A parsed member: dimension name plus an optional time grain, with the model
/// prefix stripped. `"orders.order_date.month"` → `("order_date", Month)`;
/// `"status"` or `"orders.status"` → `("status", None)`.
struct Member<'a> {
    name: &'a str,
    grain: Option<TimeGrain>,
}

/// Parse a query member (model-prefixed) into its dimension name + grain.
fn parse_query_dim(member: &str) -> Member<'_> {
    // Drop the leading `model.` segment, then interpret what remains.
    let rest = member.split_once('.').map_or(member, |(_, r)| r);
    parse_bare_dim(rest)
}

/// Parse a pre-agg dimension entry (already model-less), e.g. `"order_date.day"`.
fn parse_bare_dim(s: &str) -> Member<'_> {
    match s.split_once('.') {
        Some((name, grain)) => Member {
            name,
            grain: TimeGrain::parse(grain),
        },
        None => Member {
            name: s,
            grain: None,
        },
    }
}

/// The measure name from a member, model prefix stripped: `"orders.revenue"`
/// → `"revenue"`.
fn measure_name(member: &str) -> &str {
    member.rsplit('.').next().unwrap_or(member)
}

/// True when a pre-agg dimension at `pre` can serve a query dimension at `q`.
fn grain_covers(pre: Option<TimeGrain>, q: Option<TimeGrain>) -> bool {
    match (pre, q) {
        // Ungrained query dim needs the raw column grouped ungrained.
        (None, None) => true,
        // A grained rollup can't reconstruct the raw (finer) value.
        (Some(_), None) => false,
        // A raw column can always be truncated to any grain.
        (None, Some(_)) => true,
        // Both grained: the rollup must be at least as fine as the query.
        (Some(p), Some(qg)) => p.can_roll_up_to(qg),
    }
}

/// Does `pre` cover every dimension, measure, and filtered member in `q`?
fn covers(pre: &PreAggregation, q: &SemanticQuery) -> bool {
    let pre_dims: Vec<Member<'_>> = pre.dimensions.iter().map(|d| parse_bare_dim(d)).collect();

    let dim_covered = |want: &Member<'_>| {
        pre_dims
            .iter()
            .any(|have| have.name == want.name && grain_covers(have.grain, want.grain))
    };

    for member in &q.dimensions {
        if !dim_covered(&parse_query_dim(member)) {
            return false;
        }
    }
    // Filters may only touch dimensions the rollup carries (by name; a filter
    // can be applied at any grain the rollup exposes, so name presence is the
    // bar here).
    for f in &q.filters {
        let name = parse_query_dim(&f.member).name;
        if !pre_dims.iter().any(|d| d.name == name) {
            return false;
        }
    }
    for member in &q.measures {
        let m = measure_name(member);
        if !pre.measures.iter().any(|pm| pm == m) {
            return false;
        }
    }
    true
}

/// The smallest pre-aggregation on `model` that covers `q`, if any. "Smallest"
/// = fewest dimensions, so the cheapest covering rollup wins; ties resolve to
/// declaration order for determinism.
#[must_use]
pub fn match_rollup<'a>(model: &'a SemanticModel, q: &SemanticQuery) -> Option<&'a PreAggregation> {
    model
        .pre_aggregations
        .iter()
        .filter(|pre| covers(pre, q))
        .min_by_key(|pre| pre.dimensions.len())
}

/// The synthetic table name a materialized rollup is registered under:
/// `semantic_<model>__<preagg>`. The cache-layer materializer writes Parquet here.
#[must_use]
pub fn rollup_table_name(model: &str, preagg: &str) -> String {
    format!("semantic_{model}__{preagg}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_core::semantic::{FilterOp, SemanticFilter};

    fn preagg(name: &str, dims: &[&str], measures: &[&str]) -> PreAggregation {
        PreAggregation {
            name: name.into(),
            dimensions: dims.iter().map(|s| (*s).into()).collect(),
            measures: measures.iter().map(|s| (*s).into()).collect(),
            refresh: None,
            partition_by: None,
        }
    }

    fn model_with(pre: Vec<PreAggregation>) -> SemanticModel {
        SemanticModel {
            name: "orders".into(),
            description: None,
            source: "shop.orders".into(),
            primary_key: vec![],
            dimensions: vec![],
            measures: vec![],
            relationships: vec![],
            pre_aggregations: pre,
            safety: None,
        }
    }

    fn query(measures: &[&str], dims: &[&str]) -> SemanticQuery {
        SemanticQuery {
            measures: measures.iter().map(|s| (*s).into()).collect(),
            dimensions: dims.iter().map(|s| (*s).into()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn exact_dims_and_measures_cover() {
        let m = model_with(vec![preagg(
            "daily",
            &["order_date.day", "status"],
            &["revenue", "order_count"],
        )]);
        let q = query(
            &["orders.revenue"],
            &["orders.order_date.day", "orders.status"],
        );
        assert_eq!(match_rollup(&m, &q).map(|p| p.name.as_str()), Some("daily"));
    }

    #[test]
    fn coarser_grain_rolls_up_from_finer() {
        let m = model_with(vec![preagg("daily", &["order_date.day"], &["revenue"])]);
        // Query asks for month; a day rollup can be re-truncated to month.
        let q = query(&["orders.revenue"], &["orders.order_date.month"]);
        assert!(match_rollup(&m, &q).is_some());
    }

    #[test]
    fn finer_grain_not_covered_by_coarser() {
        let m = model_with(vec![preagg("monthly", &["order_date.month"], &["revenue"])]);
        // Query asks for day; a month rollup has lost the daily detail.
        let q = query(&["orders.revenue"], &["orders.order_date.day"]);
        assert!(match_rollup(&m, &q).is_none());
    }

    #[test]
    fn missing_measure_not_covered() {
        let m = model_with(vec![preagg("daily", &["status"], &["revenue"])]);
        let q = query(&["orders.order_count"], &["orders.status"]);
        assert!(match_rollup(&m, &q).is_none());
    }

    #[test]
    fn extra_query_dim_not_covered() {
        let m = model_with(vec![preagg("daily", &["status"], &["revenue"])]);
        let q = query(&["orders.revenue"], &["orders.status", "orders.country"]);
        assert!(match_rollup(&m, &q).is_none());
    }

    #[test]
    fn filter_on_uncovered_dim_disqualifies() {
        let m = model_with(vec![preagg("daily", &["status"], &["revenue"])]);
        let mut q = query(&["orders.revenue"], &["orders.status"]);
        q.filters = vec![SemanticFilter {
            member: "orders.country".into(),
            op: FilterOp::Equals,
            values: vec!["US".into()],
        }];
        assert!(match_rollup(&m, &q).is_none());
    }

    #[test]
    fn smallest_covering_rollup_wins() {
        let m = model_with(vec![
            preagg(
                "wide",
                &["status", "country", "order_date.day"],
                &["revenue"],
            ),
            preagg("narrow", &["status"], &["revenue"]),
        ]);
        let q = query(&["orders.revenue"], &["orders.status"]);
        assert_eq!(
            match_rollup(&m, &q).map(|p| p.name.as_str()),
            Some("narrow")
        );
    }

    #[test]
    fn week_does_not_roll_up_to_month() {
        let m = model_with(vec![preagg("weekly", &["order_date.week"], &["revenue"])]);
        let q = query(&["orders.revenue"], &["orders.order_date.month"]);
        assert!(match_rollup(&m, &q).is_none());
    }

    #[test]
    fn table_name_format() {
        assert_eq!(
            rollup_table_name("orders", "daily"),
            "semantic_orders__daily"
        );
    }
}
