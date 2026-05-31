//! `pawrly semantic` — browse and query the semantic layer.
//!
//! Three subcommands, each wired through `EngineService`:
//! - `list` → `list_semantic_models`
//! - `describe <model>` → `describe_semantic_model`
//! - `query <model.measure>... [--by ...] [--where ...]` → `semantic_query`
//!
//! `explain` (compiled SQL) needs a dedicated engine RPC and `refresh` drives
//! pre-aggregation materialization (handled by the cache layer); both are
//! follow-ups.

use std::collections::HashMap;
use std::path::PathBuf;

use clap::{Args as ClapArgs, Subcommand};
use pawrly_core::EngineServiceExt as _;
use pawrly_core::semantic::{FilterOp, OrderDir, SemanticFilter, SemanticOrder, SemanticQuery};

use crate::commands::format::Format;

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: SemanticCommand,
}

#[derive(Subcommand, Debug)]
pub enum SemanticCommand {
    /// List semantic models with dimension/measure counts.
    List(ListArgs),
    /// Show one model's dimensions, measures, and relationships.
    Describe(DescribeArgs),
    /// Run a structured query: measures by dimensions, with filters.
    Query(QueryArgs),
}

#[derive(ClapArgs, Debug)]
pub struct ListArgs {
    /// Emit JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(ClapArgs, Debug)]
pub struct DescribeArgs {
    /// Model name.
    #[arg(value_name = "MODEL")]
    pub model: String,
    /// Emit JSON instead of a human-readable summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(ClapArgs, Debug)]
pub struct QueryArgs {
    /// Measure members, e.g. `orders.revenue`.
    #[arg(value_name = "MEASURE")]
    pub measures: Vec<String>,

    /// Dimension members to group by, e.g. `orders.status` or
    /// `orders.order_date.month`. Repeatable.
    #[arg(long = "by", value_name = "DIM")]
    pub by: Vec<String>,

    /// Filter, as `'<member> <op> <value>'` — e.g. `'orders.status = paid'`,
    /// `'orders.country in US,CA'`, `'orders.total >= 100'`,
    /// `'orders.notes is_null'`. Repeatable.
    #[arg(long = "where", value_name = "PREDICATE")]
    pub wheres: Vec<String>,

    /// Order by a selected member; append `:desc` for descending. Repeatable.
    #[arg(long = "order-by", value_name = "MEMBER")]
    pub order_by: Vec<String>,

    /// Bind a `${param:NAME}` placeholder (RLS), as `name=value`. Repeatable.
    #[arg(long = "param", value_name = "NAME=VALUE")]
    pub params: Vec<String>,

    /// Row limit.
    #[arg(long)]
    pub limit: Option<u64>,

    /// Time zone for grain truncation, e.g. `America/New_York`.
    #[arg(long = "time-zone")]
    pub time_zone: Option<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Table)]
    pub format: Format,
}

pub async fn run(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: Args,
) -> anyhow::Result<()> {
    match args.command {
        SemanticCommand::List(a) => list(home, config, remote, no_remote, a).await,
        SemanticCommand::Describe(a) => describe(home, config, remote, no_remote, a).await,
        SemanticCommand::Query(a) => query(home, config, remote, no_remote, a).await,
    }
}

async fn list(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: ListArgs,
) -> anyhow::Result<()> {
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let models = svc.list_semantic_models().await?;

    if args.json {
        println!("{}", serde_json::to_string(&models)?);
        return Ok(());
    }
    if models.is_empty() {
        println!("no semantic models defined");
        return Ok(());
    }
    println!("{:<24} {:<6} {:<8} source", "model", "dims", "measures");
    for m in &models {
        println!(
            "{:<24} {:<6} {:<8} {}",
            m.name, m.dimension_count, m.measure_count, m.source
        );
    }
    Ok(())
}

async fn describe(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: DescribeArgs,
) -> anyhow::Result<()> {
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let m = svc.describe_semantic_model(&args.model).await?;

    if args.json {
        println!("{}", serde_json::to_string(&m)?);
        return Ok(());
    }
    println!("model:  {}", m.name);
    if let Some(d) = &m.description {
        println!("        {d}");
    }
    println!("source: {}", m.source);
    if !m.primary_key.is_empty() {
        println!("key:    {}", m.primary_key.join(", "));
    }
    println!("\ndimensions:");
    for d in &m.dimensions {
        let grains = if d.time_grains.is_empty() {
            String::new()
        } else {
            let gs: Vec<&str> = d.time_grains.iter().map(|g| g.as_str()).collect();
            format!(" [{}]", gs.join(", "))
        };
        println!("  {:<20} {:?}{}", d.name, d.data_type, grains);
    }
    println!("\nmeasures:");
    for ms in &m.measures {
        println!("  {:<20} {}", ms.name, ms.agg.label());
    }
    if !m.relationships.is_empty() {
        println!("\nrelationships:");
        for r in &m.relationships {
            println!("  {:<20} -> {} ({:?})", r.name, r.target_model, r.kind);
        }
    }
    Ok(())
}

async fn query(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: QueryArgs,
) -> anyhow::Result<()> {
    if args.measures.is_empty() && args.by.is_empty() {
        return Err(anyhow::anyhow!(
            "provide at least one measure or `--by` dimension"
        ));
    }

    let filters = args
        .wheres
        .iter()
        .map(|w| parse_filter(w))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let order_by = args.order_by.iter().map(|o| parse_order(o)).collect();
    let params = parse_params(&args.params)?;

    let q = SemanticQuery {
        measures: args.measures,
        dimensions: args.by,
        filters,
        order_by,
        limit: args.limit,
        time_zone: args.time_zone,
        params,
    };

    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let batches = svc.semantic_query_collect(q).await?;
    args.format
        .write_batches(&mut std::io::stdout(), &batches)?;
    Ok(())
}

/// Parse `name=value` pairs for `--param`.
fn parse_params(raw: &[String]) -> anyhow::Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for p in raw {
        let (k, v) = p
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--param must be `name=value`, got `{p}`"))?;
        out.insert(k.trim().to_string(), v.to_string());
    }
    Ok(out)
}

/// Parse `member[:desc]` into a [`SemanticOrder`].
fn parse_order(s: &str) -> SemanticOrder {
    match s.rsplit_once(':') {
        Some((member, dir)) if dir.eq_ignore_ascii_case("desc") => SemanticOrder {
            member: member.to_string(),
            direction: OrderDir::Desc,
        },
        Some((member, dir)) if dir.eq_ignore_ascii_case("asc") => SemanticOrder {
            member: member.to_string(),
            direction: OrderDir::Asc,
        },
        _ => SemanticOrder {
            member: s.to_string(),
            direction: OrderDir::Asc,
        },
    }
}

/// Parse a `--where` predicate `'<member> <op> <value>'` into a
/// [`SemanticFilter`]. Values for `in` / `not_in` / `in_range` are
/// comma-separated; `is_null` / `is_not_null` take no value.
fn parse_filter(s: &str) -> anyhow::Result<SemanticFilter> {
    let mut parts = s.split_whitespace();
    let member = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty --where predicate"))?
        .to_string();
    let op_token = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("--where `{s}` is missing an operator"))?;
    let rest = parts.collect::<Vec<_>>().join(" ");

    let op = parse_op(op_token)
        .ok_or_else(|| anyhow::anyhow!("--where `{s}` has unknown operator `{op_token}`"))?;

    let values = match op {
        FilterOp::IsNull | FilterOp::IsNotNull => Vec::new(),
        FilterOp::In | FilterOp::NotIn | FilterOp::InRange => rest
            .split(',')
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect(),
        _ => {
            if rest.is_empty() {
                return Err(anyhow::anyhow!("--where `{s}` is missing a value"));
            }
            vec![rest]
        }
    };
    Ok(SemanticFilter { member, op, values })
}

fn parse_op(token: &str) -> Option<FilterOp> {
    Some(match token.to_ascii_lowercase().as_str() {
        "=" | "==" | "eq" | "equals" => FilterOp::Equals,
        "!=" | "<>" | "ne" => FilterOp::NotEquals,
        ">" | "gt" => FilterOp::Gt,
        ">=" | "gte" => FilterOp::Gte,
        "<" | "lt" => FilterOp::Lt,
        "<=" | "lte" => FilterOp::Lte,
        "in" => FilterOp::In,
        "not_in" | "not-in" => FilterOp::NotIn,
        "in_range" | "between" => FilterOp::InRange,
        "contains" => FilterOp::Contains,
        "starts_with" => FilterOp::StartsWith,
        "ends_with" => FilterOp::EndsWith,
        "is_null" => FilterOp::IsNull,
        "is_not_null" => FilterOp::IsNotNull,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_equals_filter() {
        let f = parse_filter("orders.status = paid").unwrap();
        assert_eq!(f.member, "orders.status");
        assert_eq!(f.op, FilterOp::Equals);
        assert_eq!(f.values, vec!["paid"]);
    }

    #[test]
    fn parses_in_filter_comma_separated() {
        let f = parse_filter("orders.country in US, CA, MX").unwrap();
        assert_eq!(f.op, FilterOp::In);
        assert_eq!(f.values, vec!["US", "CA", "MX"]);
    }

    #[test]
    fn parses_is_null_without_value() {
        let f = parse_filter("orders.notes is_null").unwrap();
        assert_eq!(f.op, FilterOp::IsNull);
        assert!(f.values.is_empty());
    }

    #[test]
    fn comparison_value_keeps_spaces() {
        let f = parse_filter("orders.label = the long value").unwrap();
        assert_eq!(f.values, vec!["the long value"]);
    }

    #[test]
    fn unknown_op_errors() {
        assert!(parse_filter("orders.x frobnicate 1").is_err());
    }

    #[test]
    fn missing_value_errors() {
        assert!(parse_filter("orders.status =").is_err());
    }

    #[test]
    fn parses_order_desc() {
        let o = parse_order("orders.revenue:desc");
        assert_eq!(o.member, "orders.revenue");
        assert_eq!(o.direction, OrderDir::Desc);
    }

    #[test]
    fn order_defaults_to_asc() {
        let o = parse_order("orders.status");
        assert_eq!(o.direction, OrderDir::Asc);
    }

    #[test]
    fn parses_params() {
        let p = parse_params(&["tenant_id=acme".into(), "region=US".into()]).unwrap();
        assert_eq!(p.get("tenant_id").map(String::as_str), Some("acme"));
        assert_eq!(p.get("region").map(String::as_str), Some("US"));
    }

    #[test]
    fn bad_param_errors() {
        assert!(parse_params(&["noequals".into()]).is_err());
    }
}
