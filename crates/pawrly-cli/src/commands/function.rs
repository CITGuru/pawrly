//! `pawrly function list|describe|call` — table-valued function discovery and
//! invocation, wired through `EngineService`.
//!
//! `call` fetches the declaration, orders the positional/named literals into the
//! declared call order, composes `SELECT * FROM ns.name(...)` via the shared
//! [`pawrly_core::render_call_sql`] renderer, and runs it through the normal
//! query path.

use std::path::PathBuf;

use clap::{Args as ClapArgs, Subcommand};
use pawrly_core::{CallArg, EngineServiceExt as _, FunctionDescription, render_call_sql};

use crate::commands::format::Format;

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: FunctionCommand,
}

#[derive(Subcommand, Debug)]
pub enum FunctionCommand {
    /// List available functions (builtins + declared).
    List(ListArgs),
    /// Describe one function by `namespace.name`.
    Describe(DescribeArgs),
    /// Call a function and print its rows.
    Call(CallArgs),
}

#[derive(ClapArgs, Debug)]
pub struct ListArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(ClapArgs, Debug)]
pub struct DescribeArgs {
    /// `namespace.name`, e.g. `github.search_issues`.
    pub function: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(ClapArgs, Debug)]
pub struct CallArgs {
    /// `namespace.name`, e.g. `github.search_issues`.
    pub function: String,
    /// Positional arguments, in the declared order.
    pub args: Vec<String>,
    /// Named arguments (`--arg name=value`), reordered positionally.
    #[arg(long = "arg", value_name = "NAME=VALUE")]
    pub named: Vec<String>,
    /// Row cap (appended as `LIMIT`).
    #[arg(long)]
    pub limit: Option<usize>,
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
        FunctionCommand::List(a) => list(home, config, remote, no_remote, a).await,
        FunctionCommand::Describe(a) => describe(home, config, remote, no_remote, a).await,
        FunctionCommand::Call(a) => call(home, config, remote, no_remote, a).await,
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
    let functions = svc.list_functions().await?;

    if args.json {
        println!("{}", serde_json::to_string(&functions)?);
        return Ok(());
    }
    if functions.is_empty() {
        println!("no functions available");
        return Ok(());
    }
    println!(
        "{:<28} {:<6} {:<8} signature",
        "function", "kind", "builtin"
    );
    for f in &functions {
        println!(
            "{:<28} {:<6} {:<8} {}",
            format!("{}.{}", f.namespace, f.name),
            f.kind.as_str(),
            if f.builtin { "yes" } else { "no" },
            f.signature
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
    let (ns, name) = split_qualified(&args.function)?;
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let d = svc.describe_function(ns, name).await?;

    if args.json {
        println!("{}", serde_json::to_string(&d)?);
        return Ok(());
    }
    println!("function: {}", d.signature);
    println!("kind:     {}", d.kind.as_str());
    if let Some(desc) = &d.description {
        println!("          {desc}");
    }
    if !d.args.is_empty() {
        println!("\narguments:");
        for a in &d.args {
            let req = if a.required { " (required)" } else { "" };
            let def = a
                .default
                .as_ref()
                .map(|x| format!(" = {x}"))
                .unwrap_or_default();
            println!("  {:<16} {}{}{}", a.name, a.r#type, def, req);
        }
    }
    println!("\nreturns:");
    for c in &d.returns {
        println!("  {:<16} {}", c.name, c.r#type);
    }
    if !d.examples.is_empty() {
        println!("\nexamples:");
        for e in &d.examples {
            println!("  {e}");
        }
    }
    Ok(())
}

async fn call(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: CallArgs,
) -> anyhow::Result<()> {
    let (ns, name) = split_qualified(&args.function)?;
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let decl = svc.describe_function(ns, name).await?;

    let call_args = order_args(&decl, &args.args, &args.named)?;
    let mut sql = render_call_sql(ns, name, &call_args);
    if let Some(n) = args.limit {
        sql.push_str(&format!(" LIMIT {n}"));
    }

    let batches = svc.query_collect(&sql).await?;
    let mut out = std::io::stdout();
    args.format.write_batches(&mut out, &batches)?;
    Ok(())
}

/// Order positional + named (`name=value`) args into the declaration's call
/// order, filling gaps with declared defaults; a missing required arg errors.
fn order_args(
    decl: &FunctionDescription,
    positional: &[String],
    named: &[String],
) -> anyhow::Result<Vec<CallArg>> {
    let mut values: Vec<Option<String>> = vec![None; decl.args.len()];
    for (i, v) in positional.iter().enumerate() {
        if i >= values.len() {
            anyhow::bail!(
                "function `{}.{}` takes at most {} argument(s)",
                decl.namespace,
                decl.name,
                decl.args.len()
            );
        }
        values[i] = Some(v.clone());
    }
    for pair in named {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--arg must be `name=value`, got `{pair}`"))?;
        let idx = decl
            .args
            .iter()
            .position(|a| a.name == k)
            .ok_or_else(|| anyhow::anyhow!("unknown argument `{k}`"))?;
        values[idx] = Some(v.to_string());
    }

    // Render up to the last provided arg; fill any earlier gaps with defaults.
    let last = values.iter().rposition(Option::is_some);
    let mut out = Vec::new();
    if let Some(last) = last {
        for (i, slot) in values.iter().enumerate().take(last + 1) {
            let arg = &decl.args[i];
            let value = slot
                .clone()
                .or_else(|| arg.default.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "missing required argument `{}` (position {})",
                        arg.name,
                        i + 1
                    )
                })?;
            out.push(CallArg::new(value, &arg.r#type));
        }
    }
    Ok(out)
}

fn split_qualified(s: &str) -> anyhow::Result<(&str, &str)> {
    s.split_once('.')
        .filter(|(ns, n)| !ns.is_empty() && !n.is_empty())
        .ok_or_else(|| anyhow::anyhow!("expected `namespace.name`, got `{s}`"))
}
