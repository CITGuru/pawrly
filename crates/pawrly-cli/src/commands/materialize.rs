//! `pawrly materialize` — persist a query result, file, or URL as a named,
//! self-backed table addressable as `<namespace>.materialized.<name>`. Prints
//! the artifact path, row count, and size; `--drop` removes one instead.

use std::collections::HashMap;
use std::path::PathBuf;

use clap::{Args as ClapArgs, ValueEnum};
use pawrly_core::{MaterializeFormat, MaterializeSpec};

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum CliFormat {
    Parquet,
    Csv,
    Json,
}

impl From<CliFormat> for MaterializeFormat {
    fn from(f: CliFormat) -> Self {
        match f {
            CliFormat::Parquet => MaterializeFormat::Parquet,
            CliFormat::Csv => MaterializeFormat::Csv,
            CliFormat::Json => MaterializeFormat::Json,
        }
    }
}

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Name for the materialized table (queryable as
    /// `<namespace>.materialized.<name>`).
    #[arg(value_name = "NAME")]
    pub name: String,

    /// SQL whose result is persisted. One of SQL / --file / --url is required
    /// (unless --drop).
    #[arg(value_name = "SQL")]
    pub sql: Option<String>,

    /// Materialize a local file (CSV/Parquet/JSON) instead of a query.
    #[arg(long, value_name = "PATH", conflicts_with = "sql")]
    pub file: Option<PathBuf>,

    /// Materialize a remote `http(s)://` file (via DuckDB httpfs).
    #[arg(long, value_name = "URL", conflicts_with_all = ["sql", "file"])]
    pub url: Option<String>,

    /// Format for --file / --url. Inferred from the extension when omitted.
    #[arg(long, value_enum)]
    pub format: Option<CliFormat>,

    /// Drop the materialized table `<name>` instead of creating it.
    #[arg(long)]
    pub drop: bool,

    /// Materialize namespace to target (queryable as
    /// `<ns>.materialized.<name>`). Defaults to the workspace namespace.
    #[arg(long, value_name = "NS")]
    pub namespace: Option<String>,

    /// Substitute `${param:KEY}` in the SQL. Repeatable: `--param key=value`.
    #[arg(long = "param", value_name = "KEY=VALUE")]
    pub params: Vec<String>,

    /// Emit JSON instead of a human-readable summary.
    #[arg(long)]
    pub json: bool,
}

pub async fn run(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: Args,
) -> anyhow::Result<()> {
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;

    let namespace = args.namespace.as_deref();
    let address = |name: &str| match namespace {
        Some(ns) => format!("{ns}.materialized.{name}"),
        None => format!("materialized.{name}"),
    };

    if args.drop {
        if args.sql.is_some() {
            return Err(anyhow::anyhow!("--drop takes only a NAME, not a SQL query"));
        }
        let removed = svc.drop_materialized(&args.name, namespace).await?;
        if args.json {
            println!(
                "{}",
                serde_json::json!({ "name": args.name, "dropped": removed })
            );
        } else if removed {
            println!("dropped {}", address(&args.name));
        } else {
            println!("no materialized table named `{}`", address(&args.name));
        }
        return Ok(());
    }

    let spec = build_spec(&args)?;
    let outcome = svc.materialize(&args.name, spec, namespace).await?;

    if args.json {
        println!("{}", serde_json::to_string(&outcome)?);
    } else {
        println!("materialized {}", address(&args.name));
        println!("  rows:  {}", outcome.row_count);
        println!("  size:  {} bytes", outcome.size_bytes);
        println!("  path:  {}", outcome.file_path.display());
        println!("  query: SELECT … FROM {}", address(&args.name));
    }
    Ok(())
}

/// Build a `MaterializeSpec` from exactly one of SQL / --file / --url.
fn build_spec(args: &Args) -> anyhow::Result<MaterializeSpec> {
    let format = args.format.map(Into::into);
    match (&args.sql, &args.file, &args.url) {
        (Some(sql), None, None) => Ok(MaterializeSpec::Query {
            sql: sql.clone(),
            params: parse_params(&args.params)?,
        }),
        (None, Some(path), None) => Ok(MaterializeSpec::File {
            path: path.clone(),
            format,
        }),
        (None, None, Some(url)) => Ok(MaterializeSpec::Url {
            url: url.clone(),
            format,
        }),
        (None, None, None) => Err(anyhow::anyhow!(
            "provide a SQL query, --file, or --url to materialize (or use --drop)"
        )),
        _ => Err(anyhow::anyhow!(
            "provide only one of: SQL query, --file, --url"
        )),
    }
}

/// Parse repeated `key=value` pairs into a param map.
fn parse_params(raw: &[String]) -> anyhow::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for p in raw {
        let (k, v) = p
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid --param `{p}`; expected key=value"))?;
        map.insert(k.to_string(), v.to_string());
    }
    Ok(map)
}
