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

    /// Secret name (in the configured backend) holding the bearer token.
    /// Required for non-loopback TCP. Only the bind-time check is enforced.
    #[arg(long)]
    pub bearer_token_from: Option<String>,

    /// Idle timeout (humantime, e.g. `30m`). 0 means never.
    #[arg(long)]
    pub idle_timeout: Option<String>,

    /// PID file path.
    #[arg(long)]
    pub pid_file: Option<PathBuf>,
}

pub async fn run(home: Option<PathBuf>, config: Option<PathBuf>, args: Args) -> anyhow::Result<()> {
    let engine = if config.is_some() {
        build_local(config).await?
    } else {
        local_engine_placeholder().await?
    };
    let mut builder = pawrly_server::ServerBuilder::new(engine);
    if let Some(token_name) = &args.bearer_token_from {
        // The intent is recorded but enforcement is not yet wired.
        tracing::warn!(
            ?token_name,
            "bearer-token-from is recorded but not yet enforced"
        );
        builder = builder.auth(pawrly_server::AuthMode::Bearer {
            token: format!("placeholder-{token_name}"),
        });
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

fn write_pid_file(path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{}\n", std::process::id()))?;
    Ok(())
}
