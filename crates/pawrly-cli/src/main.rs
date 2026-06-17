//! Pawrly CLI entry point.

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

    /// Log output format: text | json.
    #[arg(long, env = "PAWRLY_LOG_FORMAT", global = true, default_value = "text")]
    log_format: LogFormat,

    /// OTLP collector endpoint. When set, enables OpenTelemetry trace + log export.
    #[arg(long, env = "PAWRLY_OTEL_ENDPOINT", global = true)]
    otel_endpoint: Option<String>,

    /// OTLP transport: grpc | http.
    #[arg(long, env = "PAWRLY_OTEL_PROTOCOL", global = true, default_value = "grpc")]
    otel_protocol: OtelProtocol,

    /// Serve a Prometheus `/metrics` pull endpoint at this address (e.g.
    /// `127.0.0.1:9090`). Independent of `--otel-endpoint`.
    #[arg(long, env = "PAWRLY_PROMETHEUS_LISTEN", global = true)]
    prometheus_listen: Option<std::net::SocketAddr>,

    #[command(subcommand)]
    command: Command,
}

/// CLI mirror of [`pawrly_telemetry::LogFormat`], kept local so the telemetry
/// crate need not depend on clap.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

impl From<LogFormat> for pawrly_telemetry::LogFormat {
    fn from(value: LogFormat) -> Self {
        match value {
            LogFormat::Text => pawrly_telemetry::LogFormat::Text,
            LogFormat::Json => pawrly_telemetry::LogFormat::Json,
        }
    }
}

/// CLI mirror of [`pawrly_telemetry::OtelProtocol`].
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum OtelProtocol {
    Grpc,
    Http,
}

impl From<OtelProtocol> for pawrly_telemetry::OtelProtocol {
    fn from(value: OtelProtocol) -> Self {
        match value {
            OtelProtocol::Grpc => pawrly_telemetry::OtelProtocol::Grpc,
            OtelProtocol::Http => pawrly_telemetry::OtelProtocol::Http,
        }
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Create a new pawrly.yaml in the current directory.
    Init(commands::init::Args),
    /// Validate a pawrly.yaml.
    Validate(commands::validate::Args),
    /// Run each source's `examples:` statements as live probes.
    Check(commands::check::Args),
    /// Inspect the workspace config (show, with --raw / --tree).
    Config(commands::config::Args),
    /// Run a SQL query.
    Sql(commands::sql::Args),
    /// Show the SQL catalog (or describe a single table).
    Schema(commands::schema::Args),
    /// Manage the cache (list, show, refresh, invalidate, vacuum).
    Cache(commands::cache::Args),
    /// Materialize a query result as a named, self-backed table (or --drop one).
    Materialize(commands::materialize::Args),
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
    /// Run the MCP server over HTTP.
    McpHttp(commands::mcp_http::Args),
    /// Print the engine version + health.
    Version,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
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
    // Build OTel config when either OTLP export or the Prometheus pull endpoint
    // is requested. Traces/logs/metrics-push ride on the OTLP endpoint;
    // Prometheus pull is independent.
    let otel = if cli.otel_endpoint.is_some() || cli.prometheus_listen.is_some() {
        let has_otlp = cli.otel_endpoint.is_some();
        Some(pawrly_telemetry::OtelConfig {
            endpoint: cli.otel_endpoint.clone().unwrap_or_default(),
            protocol: cli.otel_protocol.into(),
            service_name: "pawrly".to_string(),
            traces: has_otlp,
            logs: has_otlp,
            metrics: has_otlp,
            sample_ratio: 1.0,
            prometheus: cli
                .prometheus_listen
                .map(|listen| pawrly_telemetry::PrometheusConfig { listen }),
        })
    } else {
        None
    };
    // Hold the guard for the whole process: it flushes the OTel exporters on
    // drop. Declared after `runtime` so it drops (and flushes) before the
    // runtime is torn down. Init runs inside the runtime so the OTLP exporters
    // can spawn their background workers.
    let _telemetry = {
        let _enter = runtime.enter();
        pawrly_telemetry::init(
            &pawrly_telemetry::TelemetryConfig {
                level: cli.log_level.clone(),
                format: cli.log_format.into(),
                otel,
            },
            role_for(&cli.command),
        )
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
        Command::Check(args) => {
            commands::check::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
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
        Command::Materialize(args) => {
            commands::materialize::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
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
        Command::McpHttp(args) => {
            commands::mcp_http::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
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

/// Pick the telemetry role from the subcommand: long-running servers report
/// their own role so logs/traces (and, later, the OTel resource) are tagged
/// correctly; everything else is a one-shot CLI invocation.
fn role_for(command: &Command) -> pawrly_telemetry::ServiceRole {
    use pawrly_telemetry::ServiceRole;
    match command {
        Command::Serve(_) => ServiceRole::Daemon,
        Command::McpStdio(_) => ServiceRole::McpStdio,
        Command::McpHttp(_) => ServiceRole::McpHttp,
        _ => ServiceRole::Cli,
    }
}

fn exit_code_for(_err: &anyhow::Error) -> u8 {
    1
}
