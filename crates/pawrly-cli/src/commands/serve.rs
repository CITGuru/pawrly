//! `pawrly serve` — run the gRPC daemon.
//!
//! Serves a `LocalEngine` over the discovered workspace manifest (`--config`,
//! `$PAWRLY_CONFIG`, `./pawrly.yaml`, or `<home>/pawrly.yaml`), falling back to
//! the engine-less placeholder when no manifest exists anywhere.

use std::path::PathBuf;

use clap::Args as ClapArgs;

use crate::engine::{build_local, local_engine_placeholder};

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Address to bind. Defaults to `unix://$PAWRLY_HOME/sockets/pawrly.sock`.
    /// Accepts `unix:///path` or `tcp://host:port`.
    #[arg(long)]
    pub addr: Option<String>,

    /// Override the UDS path directly. Equivalent to `--addr unix://<path>`.
    #[arg(long)]
    pub socket: Option<PathBuf>,

    /// Name of the bearer token to require — resolved from the config's secret
    /// backend, or an environment variable of the same name. Required for
    /// non-loopback TCP; enforced on every request.
    #[arg(long)]
    pub bearer_token_from: Option<String>,

    /// PEM certificate file to serve TLS with. Requires `--tls-key`.
    #[arg(long, requires = "tls_key")]
    pub tls_cert: Option<PathBuf>,

    /// PEM private-key file for `--tls-cert`.
    #[arg(long, requires = "tls_cert")]
    pub tls_key: Option<PathBuf>,

    /// Idle timeout (humantime, e.g. `30m`). 0 means never.
    #[arg(long)]
    pub idle_timeout: Option<String>,

    /// PID file path.
    #[arg(long)]
    pub pid_file: Option<PathBuf>,

    /// Serve the web Console (gRPC-Web + embedded UI) instead of the machine
    /// gRPC wire. Binds TCP; with `--addr` use `tcp://host:port` or `host:port`
    /// (default `127.0.0.1:8787`).
    #[arg(long)]
    pub console: bool,

    /// Allow this browser origin for the Console (standalone / cross-origin
    /// hosting). Only meaningful with `--console`.
    #[arg(long)]
    pub cors_origin: Option<String>,
}

pub async fn run(home: Option<PathBuf>, config: Option<PathBuf>, args: Args) -> anyhow::Result<()> {
    // With no explicit --config, run the same manifest discovery as the rest
    // of the CLI ($PAWRLY_CONFIG → ./pawrly.yaml → <home>/pawrly.yaml), so the
    // daemon serves the default workspace by default. Only with no manifest
    // anywhere does it fall back to the engine-less placeholder.
    let config = config.or_else(|| crate::engine::default_config_path(home.as_deref()));

    // Resolve the bearer token before the config path is consumed below.
    let auth_token = match &args.bearer_token_from {
        Some(name) => Some(resolve_bearer_token(name, config.as_deref())?),
        None => None,
    };

    let engine = if config.as_deref().is_some_and(std::path::Path::exists) {
        build_local(config, home.clone()).await?
    } else {
        local_engine_placeholder().await?
    };
    let mut builder = pawrly_server::ServerBuilder::new(engine);
    if let Some(token) = auth_token {
        builder = builder.auth(pawrly_server::AuthMode::Bearer { token });
    }
    // clap's `requires` guarantees both flags appear together.
    if let (Some(cert), Some(key)) = (&args.tls_cert, &args.tls_key) {
        builder = builder.tls(cert.clone(), key.clone());
    }

    // Console mode: gRPC-Web + embedded assets over TCP, instead of the machine
    // wire. axum::serve does not carry tonic's TLS, so `--tls-*` is ignored here
    // (front a non-loopback Console with a TLS-terminating proxy).
    if args.console {
        let addr: std::net::SocketAddr = match args.addr.as_deref() {
            Some(a) => {
                let a = a.strip_prefix("tcp://").unwrap_or(a);
                a.parse()
                    .map_err(|e| anyhow::anyhow!("bad console --addr `{a}`: {e}"))?
            }
            None => std::net::SocketAddr::from(([127, 0, 0, 1], 8787)),
        };
        if let Some(pid) = &args.pid_file {
            write_pid_file(pid)?;
        }
        tracing::info!(%addr, "starting pawrly console (gRPC-Web + assets)");
        builder
            .serve_console(pawrly_server::ConsoleOpts {
                addr,
                cors_origin: args.cors_origin.clone(),
            })
            .await?;
        return Ok(());
    }

    if let Some(addr) = args.addr.as_deref() {
        if let Some(rest) = addr
            .strip_prefix("unix://")
            .or_else(|| addr.strip_prefix("uds://"))
        {
            #[cfg(unix)]
            {
                let path = PathBuf::from(rest);
                if let Some(pid) = &args.pid_file {
                    write_pid_file(pid)?;
                }
                tracing::info!(path = %path.display(), "starting pawrly daemon (UDS)");
                serve_uds_graceful(builder, path, args.pid_file.clone()).await?;
                return Ok(());
            }
            #[cfg(not(unix))]
            anyhow::bail!("UDS not supported on this platform");
        }
        if let Some(rest) = addr.strip_prefix("tcp://") {
            let sock: std::net::SocketAddr = rest
                .parse()
                .map_err(|e| anyhow::anyhow!("bad tcp address `{rest}`: {e}"))?;
            if let Some(pid) = &args.pid_file {
                write_pid_file(pid)?;
            }
            tracing::info!(addr = %sock, "starting pawrly daemon (TCP)");
            builder.serve_tcp(sock).await?;
            return Ok(());
        }
        anyhow::bail!("unrecognized --addr `{addr}`");
    }

    // Default: UDS at $PAWRLY_HOME/sockets/pawrly.sock.
    let path = if let Some(p) = args.socket {
        p
    } else {
        default_socket_path(home.as_deref())?
    };
    if let Some(pid) = &args.pid_file {
        write_pid_file(pid)?;
    }
    tracing::info!(path = %path.display(), "starting pawrly daemon (UDS, default path)");
    #[cfg(unix)]
    {
        serve_uds_graceful(builder, path, args.pid_file.clone()).await?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = builder;
        let _ = path;
        anyhow::bail!("UDS not supported on this platform; use --addr tcp://...")
    }
}

/// Serve on a UDS, removing the socket and pid file on a clean shutdown.
///
/// On SIGTERM/SIGINT the daemon unlinks its own socket so a later `pawrly
/// status` doesn't trip over a dead socket. SIGKILL can't be caught here;
/// `pawrly stop --force` cleans up that case.
#[cfg(unix)]
async fn serve_uds_graceful(
    builder: pawrly_server::ServerBuilder,
    path: PathBuf,
    pid_file: Option<PathBuf>,
) -> anyhow::Result<()> {
    let serve = builder.serve_uds(path.clone());
    tokio::pin!(serve);
    tokio::select! {
        res = &mut serve => res?,
        () = shutdown_signal() => {
            tracing::info!(path = %path.display(), "shutdown signal received; cleaning up socket");
        }
    }
    let _ = std::fs::remove_file(&path);
    if let Some(pid) = pid_file {
        let _ = std::fs::remove_file(pid);
    }
    Ok(())
}

/// Resolve when the process receives SIGTERM or SIGINT. If a handler can't be
/// installed, this never resolves so serving continues until the transport
/// ends on its own (rather than shutting down spuriously).
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let (mut term, mut interrupt) = match (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) {
        (Ok(term), Ok(interrupt)) => (term, interrupt),
        _ => return std::future::pending().await,
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = interrupt.recv() => {}
    }
}

fn default_socket_path(home: Option<&std::path::Path>) -> anyhow::Result<PathBuf> {
    let h = pawrly_core::resolve_home(home)
        .ok_or_else(|| anyhow::anyhow!("could not resolve the Pawrly home; set $PAWRLY_HOME"))?;
    Ok(h.join("sockets").join("pawrly.sock"))
}

/// Resolve the bearer token named by `--bearer-token-from`: first the config's
/// secret backend (when a config is given — covers env / keyring / file), then
/// a plain environment variable of the same name. Errors if neither yields one,
/// since auth can't be enforced without the token.
pub(crate) fn resolve_bearer_token(
    name: &str,
    config: Option<&std::path::Path>,
) -> anyhow::Result<String> {
    if let Some(cfg) = config
        && let Some(token) = pawrly_config::resolve_secret(cfg, name)?
    {
        return Ok(token);
    }
    if let Ok(token) = std::env::var(name)
        && !token.is_empty()
    {
        return Ok(token);
    }
    anyhow::bail!(
        "bearer token `{name}` not found in the config's secret backend or the `{name}` environment variable"
    )
}

fn write_pid_file(path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{}\n", std::process::id()))?;
    Ok(())
}
