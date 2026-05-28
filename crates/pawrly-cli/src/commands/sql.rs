//! `pawrly sql` — run a one-shot SQL query.

use std::path::PathBuf;

use clap::Args as ClapArgs;
use pawrly_core::EngineServiceExt as _;

use crate::commands::format::Format;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// SQL to run. Use `-` to read from stdin.
    #[arg(value_name = "SQL")]
    pub sql: Option<String>,

    /// Read SQL from a file instead of an argument.
    #[arg(short = 'f', long)]
    pub file: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Table)]
    pub format: Format,

    /// Cap on rows shown. 0 means unlimited.
    #[arg(long, default_value = "0")]
    pub max_rows: u64,
}

pub async fn run(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: Args,
) -> anyhow::Result<()> {
    let sql = resolve_sql(&args)?;
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let mut batches = svc.query_collect(&sql).await?;
    if args.max_rows > 0 {
        truncate_in_place(&mut batches, args.max_rows as usize);
    }
    args.format
        .write_batches(&mut std::io::stdout(), &batches)?;
    Ok(())
}

fn resolve_sql(args: &Args) -> anyhow::Result<String> {
    if let Some(file) = &args.file {
        return Ok(std::fs::read_to_string(file)?);
    }
    if let Some(sql) = &args.sql {
        if sql == "-" {
            use std::io::Read as _;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            return Ok(buf);
        }
        return Ok(sql.clone());
    }
    Err(anyhow::anyhow!("provide SQL as an argument or use --file"))
}

fn truncate_in_place(batches: &mut Vec<arrow_array::RecordBatch>, max: usize) {
    let mut total = 0usize;
    let mut i = 0;
    while i < batches.len() {
        let rows = batches[i].num_rows();
        if total + rows <= max {
            total += rows;
            i += 1;
        } else if total < max {
            let keep = max - total;
            let head = batches[i].slice(0, keep);
            batches[i] = head;
            i += 1;
            batches.truncate(i);
            return;
        } else {
            batches.truncate(i);
            return;
        }
    }
}
