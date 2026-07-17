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

    /// Error output on failure: text | json. `json` prints a
    /// `{"error":{code,message}}` line to stderr for machine parsing.
    #[arg(
        long,
        env = "PAWRLY_ERROR_FORMAT",
        global = true,
        default_value = "text"
    )]
    error_format: ErrorFormat,

    /// OTLP collector endpoint. When set, enables OpenTelemetry trace + log export.
    #[arg(long, env = "PAWRLY_OTEL_ENDPOINT", global = true)]
    otel_endpoint: Option<String>,

    /// OTLP transport: grpc | http.
    #[arg(
        long,
        env = "PAWRLY_OTEL_PROTOCOL",
        global = true,
        default_value = "grpc"
    )]
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

/// How to render a fatal error on stderr.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum ErrorFormat {
    Text,
    Json,
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
    /// Inspect the workspace config (show / reload).
    Config(commands::config::Args),
    /// Run a SQL query.
    Sql(commands::sql::Args),
    /// Show the optimized (or --analyze'd) plan for a SQL string.
    Explain(commands::explain::Args),
    /// Show the SQL catalog (or describe a single table, or `snapshot`).
    Schema(commands::schema::Args),
    /// Manage the cache (list, show, refresh, invalidate, vacuum).
    Cache(commands::cache::Args),
    /// Materialize a query result as a named, self-backed table (or --drop one).
    Materialize(commands::materialize::Args),
    /// Manage workspace sources (add, list, remove, refresh, test).
    Source(commands::source::Args),
    /// Browse and query the semantic layer (list, describe, query).
    Semantic(commands::semantic::Args),
    /// Discover and call table-valued functions (list, describe, call).
    Function(commands::function::Args),
    /// Inspect and set declared source variables (list, set). To connect a
    /// source's OAuth variables, use `pawrly source connect`.
    Variables(commands::variables::Args),
    /// Run the Pawrly daemon (gRPC server).
    Serve(commands::serve::Args),
    /// Serve the web Console (gRPC-Web + embedded UI) for the workspace.
    Console(commands::console::Args),
    /// Stop a running Pawrly daemon.
    Stop(commands::stop::Args),
    /// Show running Pawrly daemons.
    Status(commands::status::Args),
    /// Run the MCP server over stdio.
    McpStdio(commands::mcp_stdio::Args),
    /// Run the MCP server over HTTP.
    McpHttp(commands::mcp_http::Args),
    /// Upgrade the installed `pawrly` binary in place.
    Update(commands::update::Args),
    /// Remove the installed `pawrly` binary (and --purge its data).
    Uninstall(commands::uninstall::Args),
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
    let telemetry_config = resolve_telemetry(&cli, load_observability(&cli).as_ref());
    // Hold the guard for the whole process: it flushes the OTel exporters on
    // drop. Declared after `runtime` so it drops (and flushes) before the
    // runtime is torn down. Init runs inside the runtime so the OTLP exporters
    // can spawn their background workers.
    let _telemetry = {
        let _enter = runtime.enter();
        pawrly_telemetry::init(&telemetry_config, role_for(&cli.command))
    };
    let error_format = cli.error_format;
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            print_error(&e, error_format);
            ExitCode::from(exit_code_for(&e))
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Init(args) => commands::init::run(args).await,
        Command::Validate(args) => commands::validate::run(cli.config, args).await,
        Command::Check(args) => {
            commands::check::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Config(args) => {
            commands::config::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Sql(args) => {
            commands::sql::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Explain(args) => {
            commands::explain::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
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
        Command::Function(args) => {
            commands::function::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Variables(args) => commands::variables::run(cli.home, args).await,
        Command::Serve(args) => commands::serve::run(cli.home, cli.config, args).await,
        Command::Console(args) => {
            commands::console::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Stop(args) => commands::stop::run(cli.home, args).await,
        Command::Status(args) => commands::status::run(cli.home, args).await,
        Command::McpStdio(args) => {
            commands::mcp_stdio::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::McpHttp(args) => {
            commands::mcp_http::run(cli.home, cli.config, cli.remote, cli.no_remote, args).await
        }
        Command::Update(args) => commands::update::run(args).await,
        Command::Uninstall(args) => commands::uninstall::run(cli.home, args).await,
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

/// Best-effort read of the `observability:` block from the workspace config,
/// without resolving secrets (which could prompt or fail) or running
/// validation — telemetry must initialize before, and independently of, the
/// command. Returns `None` if no config is found or it cannot be parsed.
fn load_observability(cli: &Cli) -> Option<pawrly_config::ObservabilityConfig> {
    /// Parse only the observability block, ignoring every other config field.
    #[derive(serde::Deserialize)]
    struct ObsOnly {
        observability: Option<pawrly_config::ObservabilityConfig>,
    }
    let path = cli.config.clone().or_else(|| {
        let default = std::path::PathBuf::from("pawrly.yaml");
        default.exists().then_some(default)
    })?;
    let text = std::fs::read_to_string(&path).ok()?;
    serde_yaml::from_str::<ObsOnly>(&text).ok()?.observability
}

/// Resolve the telemetry config from CLI flags and the config block. Flags take
/// precedence over their config-default counterparts; `RUST_LOG` still wins for
/// the level inside [`pawrly_telemetry::init`].
fn resolve_telemetry(
    cli: &Cli,
    obs: Option<&pawrly_config::ObservabilityConfig>,
) -> pawrly_telemetry::TelemetryConfig {
    // Logging: a non-default flag overrides config, which overrides the default.
    let level = if cli.log_level == "info" {
        obs.map_or_else(|| cli.log_level.clone(), |o| o.tracing.level.clone())
    } else {
        cli.log_level.clone()
    };
    let format = if matches!(cli.log_format, LogFormat::Text) {
        obs.map_or(pawrly_telemetry::LogFormat::Text, |o| {
            match o.tracing.format {
                pawrly_config::LogFormat::Text => pawrly_telemetry::LogFormat::Text,
                pawrly_config::LogFormat::Json => pawrly_telemetry::LogFormat::Json,
            }
        })
    } else {
        cli.log_format.into()
    };

    pawrly_telemetry::TelemetryConfig {
        level,
        format,
        otel: resolve_otel(cli, obs),
    }
}

/// Resolve OTLP export settings. Passing any of `--otel-endpoint` or
/// `--prometheus-listen` builds the config from flags entirely; otherwise an
/// enabled `observability.otel` block drives it. `None` means no export.
fn resolve_otel(
    cli: &Cli,
    obs: Option<&pawrly_config::ObservabilityConfig>,
) -> Option<pawrly_telemetry::OtelConfig> {
    if cli.otel_endpoint.is_some() || cli.prometheus_listen.is_some() {
        let has_otlp = cli.otel_endpoint.is_some();
        return Some(pawrly_telemetry::OtelConfig {
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
        });
    }

    let otel = obs.map(|o| &o.otel)?;
    if !otel.enabled && !otel.prometheus.enabled {
        return None;
    }
    let prometheus = otel
        .prometheus
        .enabled
        .then(|| otel.prometheus.listen.parse().ok())
        .flatten()
        .map(|listen| pawrly_telemetry::PrometheusConfig { listen });
    Some(pawrly_telemetry::OtelConfig {
        endpoint: otel.endpoint.clone(),
        protocol: match otel.protocol {
            pawrly_config::OtelProtocol::Grpc => pawrly_telemetry::OtelProtocol::Grpc,
            pawrly_config::OtelProtocol::Http => pawrly_telemetry::OtelProtocol::Http,
        },
        service_name: otel.service_name.clone(),
        traces: otel.enabled && otel.traces,
        logs: otel.enabled && otel.logs,
        metrics: otel.enabled && otel.metrics,
        sample_ratio: otel.sample_ratio,
        prometheus,
    })
}

/// Pick the telemetry role from the subcommand: long-running servers report
/// their own role so logs/traces (and, later, the OTel resource) are tagged
/// correctly; everything else is a one-shot CLI invocation.
fn role_for(command: &Command) -> pawrly_telemetry::ServiceRole {
    use pawrly_telemetry::ServiceRole;
    match command {
        Command::Serve(_) | Command::Console(_) => ServiceRole::Daemon,
        Command::McpStdio(_) => ServiceRole::McpStdio,
        Command::McpHttp(_) => ServiceRole::McpHttp,
        _ => ServiceRole::Cli,
    }
}

/// Print a fatal error to stderr, either human-readable or as a machine-parsable
/// `{"error":{code,message}}` JSON line.
fn print_error(err: &anyhow::Error, format: ErrorFormat) {
    match format {
        ErrorFormat::Text => eprintln!("error: {err}"),
        ErrorFormat::Json => {
            let line = serde_json::json!({
                "error": { "code": error_code_for(err), "message": err.to_string() }
            });
            eprintln!("{line}");
        }
    }
}

/// The stable `PAWRLY_*` code for a fatal error, drawn from the shared taxonomy
/// when the cause is a Pawrly error; `PAWRLY_INTERNAL` otherwise.
fn error_code_for(err: &anyhow::Error) -> pawrly_core::ErrorCode {
    use pawrly_core::{ConfigError, EngineError, PawrlyError, SourceError};
    if let Some(e) = err.downcast_ref::<PawrlyError>() {
        e.code()
    } else if let Some(e) = err.downcast_ref::<EngineError>() {
        e.code()
    } else if let Some(e) = err.downcast_ref::<SourceError>() {
        e.code()
    } else if let Some(e) = err.downcast_ref::<ConfigError>() {
        e.code()
    } else {
        pawrly_core::error::codes::INTERNAL
    }
}

/// Map a fatal error to a stable exit code by category (sysexits.h-aligned) so
/// scripts can branch on the failure kind instead of a blanket `1`.
fn exit_code_for(err: &anyhow::Error) -> u8 {
    use pawrly_core::{EngineError, PawrlyError};

    // Engine errors, whether raw or wrapped in a PawrlyError.
    let engine =
        err.downcast_ref::<EngineError>()
            .or_else(|| match err.downcast_ref::<PawrlyError>() {
                Some(PawrlyError::Engine(e)) => Some(e),
                _ => None,
            });
    if let Some(e) = engine {
        return match e {
            EngineError::InvalidSql(_)
            | EngineError::SemanticPlan(_)
            | EngineError::UnknownKind(_) => 65, // EX_DATAERR
            EngineError::UnknownTable(_) | EngineError::UnknownFunction(_) => 66, // EX_NOINPUT
            EngineError::Safety(_) => 77,                                         // EX_NOPERM
            EngineError::SourceRegistration { .. } => 69,                         // EX_UNAVAILABLE
            EngineError::Timeout(_) => 75,                                        // EX_TEMPFAIL
            EngineError::OutOfMemory(_) => 71,                                    // EX_OSERR
            EngineError::Cancelled => 130,                                        // terminated
            EngineError::Protocol(_) | EngineError::Internal(_) => 70,            // EX_SOFTWARE
            EngineError::Unsupported(_) => 69,                                    // EX_UNAVAILABLE
        };
    }
    if let Some(e) = err.downcast_ref::<PawrlyError>() {
        return match e {
            PawrlyError::Config(_) => 78, // EX_CONFIG
            PawrlyError::Safety(_) => 77,
            PawrlyError::Source(_) => 69,
            PawrlyError::Engine(_) => 70,
        };
    }
    1
}

#[cfg(test)]
mod tests {
    use pawrly_core::EngineError;

    use super::*;

    #[test]
    fn exit_codes_distinct_by_category() {
        assert_eq!(
            exit_code_for(&EngineError::InvalidSql("x".into()).into()),
            65
        );
        assert_eq!(
            exit_code_for(&EngineError::UnknownTable("t".into()).into()),
            66
        );
        assert_eq!(exit_code_for(&EngineError::Cancelled.into()), 130);
        assert_eq!(exit_code_for(&EngineError::Internal("x".into()).into()), 70);
        assert_eq!(exit_code_for(&anyhow::anyhow!("plain")), 1);
    }

    #[test]
    fn error_code_maps_to_taxonomy() {
        assert_eq!(
            error_code_for(&EngineError::InvalidSql("x".into()).into()),
            "PAWRLY_INVALID_SQL"
        );
        assert_eq!(error_code_for(&anyhow::anyhow!("plain")), "PAWRLY_INTERNAL");
    }
}
