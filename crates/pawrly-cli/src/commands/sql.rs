//! `pawrly sql` — run a one-shot SQL query.

use std::path::PathBuf;

use clap::Args as ClapArgs;
use pawrly_core::EngineServiceExt as _;

use crate::commands::format::Format;

/// Which engine plans and runs the SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Engine {
    /// Federated engine (DataFusion): every configured source is queryable.
    Datafusion,
    /// Embedded DuckDB: the SQL runs directly on an in-process DuckDB, bypassing
    /// federation. Only literal/self-contained data is visible — live sources
    /// (HTTP, Postgres, MCP, …) are DataFusion providers and are not available
    /// here.
    Duckdb,
}

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

    /// Shortcut for `--format json` (takes precedence over `--format`).
    #[arg(long)]
    pub json: bool,

    /// Engine to run the query on (default: `datafusion`, the federated engine).
    /// `duckdb` runs the SQL directly on an embedded DuckDB.
    #[arg(long, value_enum, default_value_t = Engine::Datafusion)]
    pub engine: Engine,

    /// Alias for `--engine duckdb`.
    #[arg(long, hide = true)]
    pub duckdb: bool,
}

pub async fn run(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: Args,
) -> anyhow::Result<()> {
    let sql = resolve_sql(&args)?;
    let engine = if args.duckdb {
        Engine::Duckdb
    } else {
        args.engine
    };
    let mut batches = match engine {
        Engine::Datafusion => {
            let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
            svc.query_collect(&sql).await?
        }
        // Embedded DuckDB: bypass federation and run the SQL straight on an
        // in-process DuckDB. Runs in-process regardless of `--remote`.
        Engine::Duckdb => pawrly_engine::DuckDbPool::new(1)?.fetch_arrow(&sql).await?,
    };
    if args.max_rows > 0 {
        truncate_in_place(&mut batches, args.max_rows as usize);
    }
    let format = if args.json { Format::Json } else { args.format };
    format.write_batches(&mut std::io::stdout(), &batches)?;
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
