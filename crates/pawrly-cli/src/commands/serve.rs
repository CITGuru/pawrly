//! `pawrly serve` — run the gRPC daemon.
//!
//! Serves the placeholder/mock engine, or a `LocalEngine` when given a config.

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
}

pub async fn run(home: Option<PathBuf>, config: Option<PathBuf>, args: Args) -> anyhow::Result<()> {
    // Resolve the bearer token before the config path is consumed below.
    let auth_token = match &args.bearer_token_from {
        Some(name) => Some(resolve_bearer_token(name, config.as_deref())?),
        None => None,
    };

    let engine = if config.is_some() {
        build_local(config).await?
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
                builder.serve_uds(path).await?;
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
        builder.serve_uds(path).await?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = builder;
        let _ = path;
        anyhow::bail!("UDS not supported on this platform; use --addr tcp://...")
    }
}

fn default_socket_path(home: Option<&std::path::Path>) -> anyhow::Result<PathBuf> {
    let h = home
        .map(|p| p.to_path_buf())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".pawrly"))
        })
        .ok_or_else(|| anyhow::anyhow!("could not resolve $PAWRLY_HOME (no $HOME)"))?;
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
