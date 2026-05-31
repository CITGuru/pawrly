//! Pawrly CLI entry point.
//!
//! Only `serve`, `stop`, `status`, and the global `--remote` /
//! `--no-remote` plumbing are wired against `MockEngine`. Real subcommands
//! that need a `LocalEngine` (`sql`, `schema`, `source`, `cache`, `mcp-*`)
//! are not yet wired.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use pawrly_core::EngineService;

mod commands;
mod engine;

#[derive(Parser, Debug)]
#[command(
    name = "pawrly",
    version,
    about = "SQL over APIs, files, and AI models",
    arg_required_else_help = true
)]
struct Cli {
    /// Path to pawrly.yaml. Overrides PAWRLY_CONFIG.
    #[arg(short = 'c', long, env = "PAWRLY_CONFIG", global = true)]
    config: Option<PathBuf>,

    /// PAWRLY_HOME override.
    #[arg(long, env = "PAWRLY_HOME", global = true)]
    home: Option<PathBuf>,

    /// Talk to a daemon at ENDPOINT instead of running in-process.
    /// Accepts: `uds:///path`, `tcp://host:port`, or `off` to force in-process.
    #[arg(long, env = "PAWRLY_REMOTE", global = true)]
    remote: Option<String>,

    /// Force in-process execution even if a local daemon is detected.
    #[arg(long, env = "PAWRLY_NO_REMOTE", global = true)]
    no_remote: bool,

    /// Logging level: error | warn | info | debug | trace.
    #[arg(long, env = "PAWRLY_LOG", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Create a new pawrly.yaml in the current directory.
    Init(commands::init::Args),
    /// Validate a pawrly.yaml.
    Validate(commands::validate::Args),
    /// Inspect the workspace config (show, with --raw / --tree).
    Config(commands::config::Args),
    /// Run a SQL query.
    Sql(commands::sql::Args),
    /// Show the SQL catalog (or describe a single table).
    Schema(commands::schema::Args),
    /// Manage the cache (list, show, refresh, invalidate, vacuum).
    Cache(commands::cache::Args),
    /// Manage workspace sources (add, list, remove, refresh, test).
    Source(commands::source::Args),
    /// Browse and query the semantic layer (list, describe, query).
    Semantic(commands::semantic::Args),
    /// Run the Pawrly daemon (gRPC server).
    Serve(commands::serve::Args),
    /// Stop a running Pawrly daemon.
    Stop(commands::stop::Args),
    /// Show running Pawrly daemons.
    Status(commands::status::Args),
    /// Run the MCP server over stdio.
    McpStdio(commands::mcp_stdio::Args),
    /// Print the engine version + health.
    Version,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    install_logging(&cli.log_level);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: failed to start tokio runtime: {e}");
            return ExitCode::from(64);
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(exit_code_for(&e))
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Init(args) => commands::init::run(args).await,
        Command::Validate(args) => commands::validate::run(args).await,
        Command::Config(args) => commands::config::run(cli.config, args).await,
        Command::Sql(args) => {
            commands::sql::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Schema(args) => {
            commands::schema::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Cache(args) => {
            commands::cache::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Source(args) => {
            commands::source::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Semantic(args) => {
            commands::semantic::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Serve(args) => commands::serve::run(cli.home, cli.config, args).await,
        Command::Stop(args) => commands::stop::run(cli.home, args).await,
        Command::Status(args) => commands::status::run(cli.home, args).await,
        Command::McpStdio(args) => {
            commands::mcp_stdio::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Version => print_version(cli.remote, cli.no_remote, cli.home, cli.config).await,
    }
}

async fn print_version(
    remote: Option<String>,
    no_remote: bool,
    home: Option<PathBuf>,
    config: Option<PathBuf>,
) -> anyhow::Result<()> {
    let svc: Arc<dyn EngineService> = engine::build_engine(remote, no_remote, home, config).await?;
    let h = svc.health().await?;
    println!(
        "pawrly {} (engine ok={}, sources_ok={})",
        h.version, h.ok, h.sources_ok
    );
    Ok(())
}

fn install_logging(level: &str) {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn exit_code_for(_err: &anyhow::Error) -> u8 {
    1
}
