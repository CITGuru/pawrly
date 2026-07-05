//! `pawrly explain` — the optimized (or `--analyze`d) plan for a SQL string.

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// SQL to explain. Use `-` to read from stdin.
    #[arg(value_name = "SQL")]
    pub sql: Option<String>,

    /// Read SQL from a file instead of an argument.
    #[arg(short = 'f', long)]
    pub file: Option<PathBuf>,

    /// Execute the plan and include runtime metrics.
    #[arg(long)]
    pub analyze: bool,

    /// Emit JSON (`{"plan": ...}`) instead of plain text.
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
    let sql = read_sql(args.sql.as_deref(), args.file.as_deref())?;
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let plan = svc.explain(&sql, args.analyze).await?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "plan": plan }))?
        );
    } else {
        println!("{plan}");
    }
    Ok(())
}

/// Resolve SQL from a positional arg (`-` = stdin) or `--file`.
fn read_sql(sql: Option<&str>, file: Option<&Path>) -> anyhow::Result<String> {
    if let Some(file) = file {
        return Ok(std::fs::read_to_string(file)?);
    }
    match sql {
        Some("-") => {
            use std::io::Read as _;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(buf)
        }
        Some(s) => Ok(s.to_string()),
        None => Err(anyhow::anyhow!("provide SQL as an argument or use --file")),
    }
}
